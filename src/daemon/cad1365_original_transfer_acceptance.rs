//! Independent CAD-1365 acceptance for guarded ORIGINAL transfer:
//! native RPC `app_media_original` and board
//! `POST /api/app-media/original`, authored from
//! `design-plans/designer-original-transport-v1.md` — NOT by the
//! implementer, who may not edit or weaken it.
//!
//! Contract under test (the doc, not code, is the spec):
//! - Native params are CLOSED: `action_token, token, key, origin,
//!   asset_id`. A successful reply on the SAME unix connection is one
//!   JSON-RPC line whose result is the closed header
//!   `{transfer:"app-media-original/1", asset_id, media_type, bytes,
//!   sha256}` followed by exactly `bytes` raw octets and connection
//!   close. Errors are ordinary JSON-RPC errors with NO binary body.
//! - HTTP body is `{action_token, asset_id}` only; the session proof is
//!   the request's own classified credential (operator cookie +
//!   `X-Cadence-Session` + classified Origin/Host). HTTP is at least as
//!   strict as the native guard; a body-selected credential/ref/scope
//!   refuses at the closed body before dispatch.
//! - Authority is the request's real operator session + the mount's
//!   media.read declaration + the live install digest/approval +
//!   active context, then the asset's private origin → verified
//!   capability receipt → approved run → frozen slot/binding/
//!   connection material — checked against the CURRENT binding/
//!   revision before any provider artifact GET. Creator is never
//!   authority: a different permitted actor reads; a foreign-install
//!   asset, drifted/revoked binding, unlinked legacy asset or
//!   withdrawn mount/context/media.read refuses BEFORE a GET.
//! - The original is the provider's retained artifact (>2 MiB, ≤10 MiB
//!   fetch budget): byte-exact delivery, MIME from verified bytes,
//!   truncated/tampered/oversize produces NO success body. No second
//!   submit/billing request rides a download; receipt/catalog counts
//!   never change.
//!
//! Baseline honesty: `app_media_original` is not in the method table
//! ("Unknown method") and `/api/app-media/original` is not a board
//! route (404) on this head — that is a genuine RED at a real missing
//! feature, never a passing negative. Every refusal check below rides
//! the same live fixture whose positive control must succeed after
//! implementation, so a blanket deny cannot masquerade as a pass.
//!
//! Fixture honesty: the provider is a loopback HTTP server speaking the
//! EXISTING AgenticOS media schema (price read, submit, artifact bytes)
//! — never a new API. Synthetic file custody only; no keychain, real
//! credential, paid provider or non-loopback network. Producing the
//! original asset uses the REAL create/approve/dispatch host pipeline
//! over a raw unix-socket request (the test builds the request frame
//! with `proto::request` + the real `caller_frame` seam the client
//! itself attaches — a protocol reader, not a new authorization).

#![cfg(all(test, feature = "test-seam"))]

use super::{ServeOptions, Shared};
use crate::platform::{agenticos_external, Custody, Key};
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type ArtifactOverride = Arc<Mutex<Option<(Vec<u8>, String)>>>;

const ORIGINAL_LIMIT: usize = 10 * 1024 * 1024;
const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";

/// The Designer-like host-execution package — same recipe the frozen
/// generation acceptance installs: one `create-image` workflow whose
/// `image` slot binds the enrolled `gen1365` connection.
const APP_MD: &str = r#"---
app: designer-fixture
title: Designer fixture
version: '1.0.0'
summary: One host-execution image-generation workflow for CAD-1365.
needs:
  connections: []
  capabilities:
    image:
      schema: 1
      capability: media.generate
      version: 1
      action: generate_image
      resource_kind: connection_account
      effect: draft
---

# Designer fixture

One declared host-execution image run.
"#;

const WORKFLOW_MD: &str = r#"---
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

const JOB_ID: &str = "med_job1365orig";

/// Agent execution with the same image slot. The host workflow stays the
/// default package; this file is written only for the lineage entry check.
const WORKER_WORKFLOW_MD: &str = r#"---
title: "Worker image: {{subject}}"
goal: "One assigned turn over the frozen image slot"
label: Worker image
capability_slots: [image]
execution: agent
inputs:
  subject: { ask: "Image subject", example: "Harbour" }
  source: { ask: "Source facts", example: "A red lighthouse marks the harbour" }
  image_prompt: { ask: "Image guidance", example: "Use an editorial palette" }
---

## Prepare the frozen image: {{subject}}
agent: image-worker
size: S
action: local.text.produce

The assigned worker calls the frozen image slot. No second provider is named.

### Acceptance
- [ ] the assigned worker used only the frozen image slot
"#;

#[path = "cad1459_native_lineage_entry_acceptance.rs"]
mod cad1459_native_lineage_entry_acceptance;
#[path = "cad1459_original_account_acceptance.rs"]
mod cad1459_original_account_acceptance;
#[path = "cad1459_public_membership_original_acceptance.rs"]
mod cad1459_public_membership_original_acceptance;

/// The >2 MiB original the provider retains — the exact byte recipe
/// `cad1365_original_artifact_acceptance` proves forces the custody
/// derivative path: deterministic SplitMix64 noise over two thirds of a
/// 1280 square PNG (incompressible) plus a flat third.
fn noisy_square_png() -> Vec<u8> {
    use image::ImageEncoder as _;
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
        png.len() > 2 * 1024 * 1024 && png.len() <= ORIGINAL_LIMIT,
        "fixture original must force the original-descriptor path ({} bytes)",
        png.len()
    );
    png
}

/// The media screen: the frozen fixture's own `app-screens/v2`
/// manifest (real client.js asset + provenance digests, copied so the
/// integrity check verifies) with only `app`, `host_contract` and `may`
/// overridden — `screen-media.v1` + `media.read` beside the nonempty
/// provider-alias tools map, the declaration the frozen contract names.
fn media_screen(app_dir: &std::path::Path) {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens/feed");
    let dst = app_dir.join("screens/media");
    std::fs::create_dir_all(&dst).unwrap();
    for entry in std::fs::read_dir(&src).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), dst.join(entry.file_name())).unwrap();
    }
    let screens = dst.join("screens.json");
    let mut decl: Value =
        serde_json::from_str(&std::fs::read_to_string(&screens).unwrap()).unwrap();
    decl["app"] = json!("designer-fixture");
    decl["host_contract"] = json!("screen-media.v1");
    decl["may"] = json!(["tools.invoke", "media.read"]);
    std::fs::write(&screens, serde_json::to_string_pretty(&decl).unwrap()).unwrap();
}

#[derive(Debug)]
struct DoorRecord {
    method: String,
    url: String,
}

/// Loopback AgenticOS media door: the EXISTING provider schema — price
/// read, submit, artifact bytes. `original` is what the artifact GET
/// serves; `descriptor` overrides let an integrity case ship a lying
/// job view while still running the real submit path.
fn media_door(
    original: Vec<u8>,
    descriptor: Option<Value>,
    get_override: ArtifactOverride,
    stop: Arc<AtomicBool>,
) -> (
    String,
    Arc<Mutex<Vec<DoorRecord>>>,
    std::thread::JoinHandle<()>,
) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let seen = Arc::new(Mutex::new(Vec::<DoorRecord>::new()));
    let seen_worker = seen.clone();
    let digest = format!("{:x}", Sha256::digest(&original));
    let artifact = descriptor.unwrap_or_else(|| {
        json!({"ref": format!("{JOB_ID}.{digest}"), "digest": digest,
            "bytes": original.len(), "mime": "image/png"})
    });
    let worker = std::thread::spawn(move || {
        // Serve until the fixture drops: `stop` is checked every poll
        // round, so teardown costs one recv_timeout instead of a fixed
        // lifetime. No fixed-duration wait — the door ends when the test
        // is done, not on a clock.
        while !stop.load(Ordering::SeqCst) {
            let request = match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(_) => break,
            };
            let (method, url) = (request.method().to_string(), request.url().to_string());
            let reply = match (method.as_str(), url.as_str()) {
                ("GET", "/v1/runtime/media/price/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5",
                            "price":{"slug":"generate_image","chargeMinor":31_500,
                            "currency":"USD","version":"2026-09-29T00:00:00.000Z"}}})
                    .to_string(),
                )
                .with_status_code(200),
                ("POST", "/v1/runtime/media/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"job":{
                            "id":JOB_ID,"kind":"image","status":"succeeded",
                            "model":"openai/gpt-image-2.5","provider":"upstream-fixture",
                            "providerTaskId":null,
                            "artifacts":[artifact],
                            "artifactError":null,"usage":{"providerCredits":null},
                            "price":{"slug":"generate_image","chargeMinor":31_500,
                                "currency":"USD","version":"2026-09-29T00:00:00.000Z"},
                            "repeated":false,"cached":false,"stale":false}}})
                    .to_string(),
                )
                .with_status_code(201),
                ("GET", u) if u.starts_with("/v1/runtime/media/jobs/") => {
                    tiny_http::Response::from_string(
                        json!({"ok":false,"error":{"code":"not_found"}}).to_string(),
                    )
                    .with_status_code(404)
                }
                ("GET", u) if u.starts_with("/v1/runtime/media/artifacts/") => {
                    // The artifact body/MIME a download test swapped
                    // in — submit/poll/descriptor stay honest, so a
                    // refusal here is the fetch/integrity guard, never
                    // fixture bookkeeping.
                    let (bytes, mime) = get_override
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or_else(|| (original.clone(), "image/png".to_string()));
                    tiny_http::Response::from_data(bytes)
                        .with_header(tiny_http::Header::from_bytes("Content-Type", mime).unwrap())
                }
                (m, u) => panic!("unexpected provider request {m} {u}"),
            };
            seen_worker.lock().unwrap().push(DoorRecord { method, url });
            request.respond(reply).unwrap();
        }
    });
    (base, seen, worker)
}

/// A fixture daemon with a real `Shared` serving on its own unix socket
/// in a temp state dir — the socket `crate::client::rpc` and the raw
/// binary reader both connect to — plus the loopback media provider,
/// a file-custody enrolled `gen1365` connection, and the installed/
/// approved/bound designer package. Custody, adapter pin and the
/// enrollment seed are set through the SAME crate-private/test-visible
/// seams `app_capabilities_rpc::tests` and the generation fixture use:
/// `Shared::new_leased` cannot run twice on one store, so custody must
/// be file-backed before `Shared::new` — `Custody::open` picks `File`
/// whenever `secret-tool` is absent; the suite contract isolates HOME
/// so the keychain probe cannot resolve. The enrollment row is written
/// through `store.platform_enroll` before the `Arc` escapes, with
/// `platform_custody` pinned to `Custody::file` inside the tempdir —
/// never a real credential.
struct Fx {
    _dir: tempfile::TempDir,
    state: std::path::PathBuf,
    shared: Arc<Shared>,
    install: String,
    context: String,
    asset_id: String,
    run_id: String,
    original: Vec<u8>,
    get_override: ArtifactOverride,
    seen: Arc<Mutex<Vec<DoorRecord>>>,
    door: Option<std::thread::JoinHandle<()>>,
    server: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    account: bool,
    worker_workflow: bool,
}

/// One raw JSON-RPC exchange on the fixture socket: the exact frame
/// `client::rpc_exchange` sends — `proto::request` plus the real seam
/// `test_caller` field — so the daemon's caller classification is the
/// production one, and the reply is parsed the same way. Used where a
/// returned `Value` is the answer; binary replies use `socket_call`
/// below.
fn rpc(fx: &Fx, who: Asserted, method: &str, params: Value) -> crate::error::Result<Value> {
    scoped(who, || crate::client::rpc(&fx.state, method, params))
}

/// One raw unix-socket request that returns the STILL-OPEN stream plus
/// the first reply line — the only honest way to assert the binary
/// contract: a JSON result header followed by exactly `bytes` octets
/// and connection close. The request frame is byte-identical to what
/// `client::rpc_exchange` writes (method/params + `test_caller`).
fn socket_call(
    fx: &Fx,
    who: Asserted,
    method: &str,
    params: Value,
) -> std::io::Result<(BufReader<UnixStream>, String)> {
    let mut request = crate::proto::request(method, params);
    if let Some(frame) = scoped(who.clone(), || crate::test_seam::caller_frame(&fx.state)).unwrap()
    {
        request[crate::test_seam::FRAME_FIELD] = frame;
    }
    let _ = who;
    let socket = fx.state.join("cadence.sock");
    let mut stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(90)))?;
    stream.write_all(format!("{request}\n").as_bytes())?;
    // Half-close the write side: the real `handle_conn` loops on
    // request lines, so clean EOF lets it close the connection after
    // this reply — while the read side still carries a future
    // original's header + raw bytes.
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok((reader, line))
}

impl Fx {
    /// `original`/`descriptor` drive the loopback provider; `None`
    /// serves the honest >2 MiB PNG with a truthful descriptor.
    fn boot(original: Vec<u8>, descriptor: Option<Value>, seed_asset: bool) -> Self {
        Self::boot_with_account(original, descriptor, seed_asset, None)
    }

    // Bounded CAD-1459 extension: register the real account adapter before
    // Shared escapes, and install synthetic TLS on each actual RPC thread.
    fn boot_with_account(
        original: Vec<u8>,
        descriptor: Option<Value>,
        seed_asset: bool,
        trust: Option<crate::issuer_transport::test_fixture::Fixture>,
    ) -> Self {
        Self::boot_configured(original, descriptor, seed_asset, trust, false, false)
    }

    /// `native_membership` captures real Native mode from provider env.
    /// `worker_workflow` adds one agent workflow beside the host image
    /// workflow. Both default off for every existing caller.
    fn boot_configured(
        original: Vec<u8>,
        descriptor: Option<Value>,
        seed_asset: bool,
        trust: Option<crate::issuer_transport::test_fixture::Fixture>,
        native_membership: bool,
        worker_workflow: bool,
    ) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1365orig")
            .tempdir()
            .unwrap();
        let (state, pm) = (dir.path().join("s"), dir.path().join("pm"));
        std::fs::create_dir_all(&state).unwrap();
        crate::issue::Pm::init(&pm).unwrap();
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: "cad1365.example.test".to_string(),
                issuer: "https://platform.example.test".to_string(),
                company: "cad1365-test-company".to_string(),
            },
        )
        .unwrap();
        crate::store::Store::open(&state.join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&crate::store::NewAgent {
                alias: "worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: state.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let get_override = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let (base, seen, door) = if trust.is_some() {
            (String::new(), Arc::new(Mutex::new(Vec::new())), None)
        } else {
            let (base, seen, door) = media_door(
                original.clone(),
                descriptor,
                get_override.clone(),
                stop.clone(),
            );
            (base, seen, Some(door))
        };
        let mut opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.provider_env.set("CADENCE_TEST_MEDIA_POLL_MS", "20");
        opts.provider_env
            .set("CADENCE_TEST_MEDIA_DEADLINE_MS", "5000");
        if native_membership {
            // Same board identity `write_config` already saved. Native mode
            // re-proves public sessions against the platform; standalone
            // callers leave these unset.
            opts.provider_env
                .set("AGENTICOS_COMPANY", "cad1365-test-company");
            opts.provider_env.set("AGENTICOS_INSTANCE", "inst0001");
            opts.provider_env
                .set("AGENTICOS_BOARD_HOST", "cad1365.example.test");
            opts.provider_env
                .set("AGENTICOS_BOARD_ISSUER", "https://platform.example.test");
            opts.provider_env
                .set("AGENTICOS_BOARD_COMPANY", "cad1365-test-company");
        }
        if trust.is_some() {
            crate::platform::account_media::attach(&mut opts).unwrap();
        } else {
            opts.platforms.insert(
                agenticos_external::PLATFORM.into(),
                Arc::new(
                    agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                        &base,
                        Some(agenticos_external::MANIFEST_PIN),
                    )
                    .unwrap(),
                ),
            );
        }
        let shared = Shared::new(&state, &opts).unwrap();
        // Custody and the enrollment row are set BEFORE the `Arc`
        // escapes — the same ordering `app_capabilities_rpc::tests`
        // uses; the fixture socket below dispatches into THIS Shared.
        let mut shared = shared;
        Arc::get_mut(&mut shared).unwrap().platform_custody =
            Custody::file(dir.path().join("fixture-custody"));
        if trust.is_none() {
            let token = b"synthetic-cad1365-orig-token";
            let key = Key {
                platform: agenticos_external::PLATFORM,
                account: ACCOUNT,
            };
            let custody = shared.platform_custody.put(&key, token).unwrap();
            shared
                .store
                .platform_enroll(
                    &crate::store::CredentialRecord {
                        connection_id: "gen1365".into(),
                        credential_revision: 1,
                        platform: agenticos_external::PLATFORM.into(),
                        account: ACCOUNT.into(),
                        scopes: vec!["provider.draft".into()],
                        fingerprint: crate::secret::fingerprint(token),
                        custody: custody.into(),
                        exchange: "token".into(),
                        enrolled_at: 1.0,
                        by: "operator".into(),
                    },
                    false,
                    None,
                )
                .unwrap();
        }
        // The listening side of the fixture socket: accept on the
        // tempdir socket and hand every connection to the REAL
        // `serve::handle_conn` — SO_PEERCRED check, `PeerUidScope`,
        // seam `scope_frame`, `Shared::dispatch` and `proto::ok`/
        // `proto::err` framing are the production path verbatim. A
        // future binary method writes through this same handler, so
        // the raw-byte assertions below can never be satisfied by a
        // fixture writer. No authorization is invented or copied.
        let listener = std::os::unix::net::UnixListener::bind(state.join("cadence.sock")).unwrap();
        {
            let shared_server = shared.clone();
            let stop_server = stop.clone();
            listener.set_nonblocking(true).unwrap();
            let request_trust = trust.clone();
            let server = std::thread::spawn(move || {
                let mut requests = Vec::new();
                loop {
                    if stop_server.load(Ordering::SeqCst) {
                        break;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let shared = shared_server.clone();
                            let trust = request_trust.clone();
                            requests.push(std::thread::spawn(move || {
                                let _tls =
                                    trust.map(crate::issuer_transport::test_fixture::install);
                                super::serve::handle_conn(shared, stream);
                            }));
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
                for request in requests {
                    request.join().expect("fixture RPC thread");
                }
            });
            let mut fx = Self {
                state,
                install: String::new(),
                context: String::new(),
                asset_id: String::new(),
                run_id: String::new(),
                original,
                get_override,
                seen,
                door,
                server: Some(server),
                stop,
                account: trust.is_some(),
                worker_workflow,
                shared,
                _dir: dir,
            };
            fx.install_package();
            if seed_asset {
                fx.produce();
            }
            fx
        }
    }

    /// Install + approve the designer package, create the active
    /// context, bind `image` to `gen1365` — all through the real RPC
    /// surface on the fixture socket.
    fn install_package(&mut self) {
        let app = self._dir.path().join("app");
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(app.join("app.md"), APP_MD).unwrap();
        std::fs::write(app.join("workflows/create-image.md"), WORKFLOW_MD).unwrap();
        media_screen(&app);
        if self.worker_workflow {
            std::fs::write(app.join("workflows/worker-image.md"), WORKER_WORKFLOW_MD).unwrap();
        }
        let installed = self
            .op(
                "app_workspace_install",
                json!({"source": app.to_str().unwrap()}),
            )
            .expect("install");
        self.install = installed["install_id"].as_str().unwrap().to_string();
        self.op(
            "app_local_install_approve",
            json!({"install_id": self.install, "digest": installed["digest"]}),
        )
        .expect("approve");
        if !self.account {
            self.op(
                "app_binding_create",
                json!({"install_id": self.install, "slot": "image",
                    "connection_id": "gen1365", "request_id": "bind-1365-orig"}),
            )
            .expect("bind image slot");
        }
        let ctx = self
            .op(
                "app_context_create",
                json!({"install_id": self.install, "label": "media",
                    "input_defaults": {}, "request_id": "ctx-1365-orig"}),
            )
            .expect("context");
        self.context = ctx["context"]["id"]
            .as_str()
            .or_else(|| ctx["id"].as_str())
            .unwrap()
            .to_string();
    }

    /// The REAL host pipeline — create → approve → dispatch — producing
    /// one retained image whose `original_artifact` is the provider's
    /// \>2 MiB PNG; the catalog asset id is read from the private
    /// origin link the producer wrote, never fabricated.
    fn produce(&mut self) {
        let (token, key) = self.session();
        let created = self
            .op(
                "app_run_create",
                json!({"install_id": self.install, "workflow": "create-image",
                    "inputs": {"subject": "Customer follow-up",
                        "source": "Acme CRM helps small teams track customers",
                        "image_prompt": "Use a calm editorial palette"},
                    "request_id": "run-1365-orig",
                    "token": token, "key": key, "origin": "loopback"}),
            )
            .expect("run create must freeze the quote");
        let id = created["id"].as_str().unwrap().to_string();
        self.run_id = id.clone();
        self.op(
            "app_run_approve",
            json!({"run_id": id, "digest": created["snapshot_digest"]}),
        )
        .expect("approve");
        let run = self
            .op("app_run_dispatch", json!({"run_id": id}))
            .expect("dispatch");
        assert_eq!(run["state"], "succeeded", "{run}");
        let (page, _next) = self.shared.store.app_media_catalog(20, None).unwrap();
        assert_eq!(page.len(), 1, "the produced run writes one catalog asset");
        self.asset_id = page[0].id.clone();
    }

    fn op(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        rpc(self, Asserted::Operator, method, params)
    }

    /// A real operator session through the native link mint + open on
    /// THIS daemon's `Auth`.
    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.state).unwrap();
        let secret = crate::operator_auth::read_secret(&self.state).unwrap();
        let nonce = self
            .op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap()["nonce"]
            .clone();
        let opened = self
            .op(
                "operator_session_open",
                json!({"nonce": nonce, "origin": "loopback"}),
            )
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_string(),
            opened["key"].as_str().unwrap().to_string(),
        )
    }

    /// Mint + consume a real mount on the media screen for `session`.
    fn mount(&self, token: &str, key: &str, generation: u64) -> (String, String) {
        let mint = self
            .op(
                "app_screen_mint",
                json!({"install_id": self.install, "context_id": self.context,
                    "tag": "media", "token": token, "key": key,
                    "origin": "loopback", "generation": generation}),
            )
            .expect("operator mint must succeed — positive control");
        let nonce = mint["mount"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        self.op("app_screen_consume", json!({"nonce": nonce}))
            .expect("operator consume must succeed — positive control");
        (nonce, mint["action_token"].as_str().unwrap().to_string())
    }

    /// Count provider artifact GETs — the fetch an original download
    /// must issue at most once, and only after every guard passed.
    fn artifact_gets(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == "GET" && r.url.starts_with("/v1/runtime/media/artifacts/"))
            .count()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
        if let Some(door) = self.door.take() {
            let _ = door.join();
        }
    }
}

/// The closed native params for `app_media_original`.
fn params(action: &str, token: &str, key: &str, asset_id: &str) -> Value {
    json!({"action_token": action, "token": token, "key": key,
        "origin": "loopback", "asset_id": asset_id})
}

/// Read the contract reply off the socket: line one is the JSON-RPC
/// result header; on success exactly `bytes` octets follow and the
/// connection closes. Returns `(header_json, body)`; `body` is empty
/// on an error reply.
fn read_transfer(reader: BufReader<UnixStream>, line: String, cap: usize) -> (Value, Vec<u8>) {
    let frame: Value =
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("reply line is not JSON: {line}"));
    let result = frame.get("result").cloned().unwrap_or(Value::Null);
    if !result.is_object() || result.get("transfer").is_none() {
        return (frame, Vec::new());
    }
    let declared = result["bytes"].as_u64().expect("header bytes") as usize;
    assert!(
        declared <= cap,
        "declared length exceeds the original bound: {declared}"
    );
    let mut body = vec![0u8; declared];
    reader
        .take(cap as u64 + 1)
        .read_exact(&mut body)
        .expect("truncated original body");
    (frame, body)
}

/// The board on a free 3110–3189 port under the fixture's stop flag —
/// the pattern the frozen HTTP-peer check proved.
fn board(fx: &Fx) -> (u16, std::thread::JoinHandle<()>) {
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    let stop = fx.stop.clone();
    let first = (std::process::id() % 80) as u16;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut offset = 0u16;
    loop {
        assert!(Instant::now() < deadline, "board startup deadline");
        let port = 3110 + (first + offset) % 80;
        let (startup, ready) = std::sync::mpsc::channel();
        let board = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            ..Default::default()
        };
        let (s, p) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &board)));
        let remaining = deadline.saturating_duration_since(Instant::now());
        match ready.recv_timeout(remaining) {
            Ok(Ok(())) => return (port, thread),
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(thread.join().is_ok());
                offset += 1;
                assert!(offset < 80, "all fixture board candidates busy");
            }
            other => {
                let _ = thread.join();
                panic!("fixture board startup failed on port {port}: {other:?}");
            }
        }
    }
}

/// POST `/api/app-media/original`, returning (status, headers, body)
/// so the binary contract (content type, disposition, exact bytes) is
/// asserted, not assumed. `session` is the operator cookie + key pair.
fn http_original(
    port: u16,
    seam: &str,
    who: &str,
    session: Option<(&str, &str)>,
    body: Value,
) -> (u16, String, String, Vec<u8>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut req = agent
        .post(format!("http://127.0.0.1:{port}/api/app-media/original"))
        .header("Host", &host)
        .header("X-Cadence-Board", "1")
        .header("Origin", format!("http://{host}"))
        .header(crate::test_seam::AS_HEADER, who)
        .header(crate::test_seam::TOKEN_HEADER, seam)
        .header("Content-Type", "application/json");
    if let Some((token, key)) = session {
        req = req
            .header("Cookie", format!("cadence_operator_{port}={token}"))
            .header("X-Cadence-Session", key);
    }
    let mut r = req.send(body.to_string()).unwrap();
    let ctype = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let disp = r
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut buf = Vec::new();
    r.body_mut()
        .as_reader()
        .take(ORIGINAL_LIMIT as u64 + 4096)
        .read_to_end(&mut buf)
        .unwrap();
    (r.status().as_u16(), ctype, disp, buf)
}

/// POSITIVE CONTROL — the spine the implementer must turn GREEN: a
/// produced asset's verified original (>2 MiB) is returned byte-exact
/// over the native socket as the closed header + exact octets + close,
/// and over the board POST as the verified image body; the derivative
/// stays custody-bounded, exactly one submit/receipt/asset/origin
/// exists, and no second submit or billing request rides a download.
/// Baseline: RED at `Unknown method 'app_media_original'` / HTTP 404 —
/// the missing feature, never a fixture error.
#[test]
fn cad1365_original_transfer_native_and_http_return_the_verified_original() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let expected_sha = format!("sha256:{:x}", Sha256::digest(&fx.original));
    let before_gets = fx.artifact_gets();

    // Native: closed params, real mount + session, raw socket reply.
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("original request reaches the daemon");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    let result = &frame["result"];
    assert_eq!(result["transfer"], "app-media-original/1", "{frame}");
    assert_eq!(result["asset_id"], fx.asset_id, "{frame}");
    assert_eq!(result["media_type"], "image/png", "{frame}");
    assert_eq!(result["bytes"], fx.original.len() as u64, "{frame}");
    assert_eq!(result["sha256"], expected_sha, "{frame}");
    assert_eq!(body, fx.original, "original is byte-exact");
    assert!(
        body.len() > 2 * 1024 * 1024,
        "the fixture exercises the >2 MiB path"
    );
    // The download fetches the artifact at most once — never a submit
    // or billing call.
    let submits = fx
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(submits, 1, "no second provider submit rode a download");
    assert!(
        fx.artifact_gets() <= before_gets + 1,
        "bounded artifact fetch"
    );

    // HTTP: the same session, closed body, verified bytes back.
    let (port, _board_thread) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let (status, ctype, disp, bytes) = http_original(
        port,
        &seam,
        "operator",
        Some((&token, &key)),
        json!({"action_token": action, "asset_id": fx.asset_id}),
    );
    assert_eq!(status, 200, "HTTP original download");
    assert_eq!(ctype, "image/png", "allowlisted image MIME: {ctype}");
    assert!(
        disp.contains("attachment"),
        "attachment disposition: {disp}"
    );
    assert!(
        disp.contains(&fx.asset_id),
        "filename derives from the asset id: {disp}"
    );
    assert_eq!(bytes, fx.original, "HTTP body is the verified original");

    // Custody derivative unchanged: receipt still names the bounded
    // derivative, and the catalog/asset/origin counts never moved.
    let receipts = fx
        .op("app_run_capability_results", json!({"run_id": fx.run_id}))
        .expect("receipts");
    let rows = receipts["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one receipt — downloads mint none");
    assert!(
        rows[0]["result"]["original_artifact"]["bytes"]
            .as_u64()
            .unwrap()
            > 2 * 1024 * 1024,
        "the receipt retains the original descriptor: {rows:?}"
    );
    let (page, _next) = fx.shared.store.app_media_catalog(20, None).unwrap();
    assert_eq!(page.len(), 1, "no catalog duplication from a download");
}

/// GROUPED REFUSAL — caller, session, mount and closed-param guards:
/// every bad case rides the SAME live positive fixture and must refuse
/// with an ordinary JSON-RPC error and NO binary body, before any
/// provider artifact GET beyond the producer's own.
#[test]
fn cad1365_original_transfer_refuses_bad_callers_and_forged_fields() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let before = fx.artifact_gets();

    // A reply is an error frame, never a success header with bytes.
    let assert_error = |reader: BufReader<UnixStream>, line: String, what: &str| {
        let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
        assert!(
            body.is_empty(),
            "{what}: refusal carried binary bytes: {frame}"
        );
        assert!(
            frame.get("error").is_some() || frame["result"].is_null(),
            "{what}: refusal must be an ordinary error frame: {frame}"
        );
    };

    // Agent and unproven callers stop at the operator gate.
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let (reader, line) = socket_call(
            &fx,
            who.clone(),
            "app_media_original",
            params(&action, &token, &key, &fx.asset_id),
        )
        .expect("socket");
        assert_error(reader, line, &format!("{who:?} original request"));
    }

    // Forged authority fields on otherwise valid params: the closed
    // param set or the session proof refuses each — no selector,
    // credential or ref field is ever authority.
    for field in [
        "creator",
        "workspace",
        "company",
        "provider_ref",
        "artifact_ref",
        "url",
        "digest",
        "sha256",
        "bytes",
        "connection_id",
        "install_id",
        "context_id",
        "caller",
        "sub",
        "as",
        "actor",
        "account",
    ] {
        let mut p = params(&action, &token, &key, &fx.asset_id);
        p[field] = json!("forged");
        let (reader, line) =
            socket_call(&fx, Asserted::Operator, "app_media_original", p).expect("socket");
        assert_error(reader, line, &format!("forged field {field}"));
    }

    // A forged session token, a forged well-formed action token, a
    // foreign session borrowing this mount, and a superseded mount
    // all refuse at the same mount/session proofs media.read runs.
    let mut forged = params(&action, &"f".repeat(64), &key, &fx.asset_id);
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        forged.clone(),
    )
    .expect("socket");
    assert_error(reader, line, "forged session token");
    forged = params(&"c".repeat(64), &token, &key, &fx.asset_id);
    let (reader, line) =
        socket_call(&fx, Asserted::Operator, "app_media_original", forged).expect("socket");
    assert_error(reader, line, "forged action token");
    let (token_b, key_b) = fx.session();
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token_b, &key_b, &fx.asset_id),
    )
    .expect("socket");
    assert_error(reader, line, "second session borrowing a mount");
    let (_n2, _action2) = fx.mount(&token, &key, 2);
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("socket");
    assert_error(reader, line, "superseded mount");

    // Refusals never fetched an original: provider artifact traffic is
    // exactly what the producer's own verified fetch already did.
    assert_eq!(
        fx.artifact_gets(),
        before,
        "a refusal must run before any original GET"
    );
}

/// Install-state and provenance refusals: a revoked approval, a
/// missing/foreign-workspace asset id and an unlinked legacy asset all
/// refuse with no original GET — the private origin → receipt →
/// approved run chain is re-proven live, never trusted from the
/// request or a bare id. The catalog is workspace-wide: nothing here
/// partitions the read by the caller's mount install.
#[test]
fn cad1365_original_transfer_refuses_revoked_foreign_and_unlinked_assets() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let before = fx.artifact_gets();

    // Missing/foreign-workspace asset ids absent from THIS store —
    // the catalog is store-wide across installs, so "foreign" here
    // means no row and no private provenance, never an install
    // partition on the read. A well-formed id with no row refuses.
    for asset_id in [
        "med-00000000000000000000000000000000",
        "med-ffffffffffffffffffffffffffffffff",
    ] {
        let (reader, line) = socket_call(
            &fx,
            Asserted::Operator,
            "app_media_original",
            params(&action, &token, &key, asset_id),
        )
        .expect("socket");
        let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
        assert!(body.is_empty(), "missing asset carried bytes: {frame}");
        assert!(
            frame.get("error").is_some() || frame["result"].is_null(),
            "missing asset must be an error frame: {frame}"
        );
    }

    // Unlinked legacy: an asset row with no private origin — the chain
    // a legacy asset cannot satisfy. Seeded through the same sealed
    // store the catalog writes on, without faking a receipt.
    fx.shared
        .store
        .write_tx(|tx| {
            tx.execute(
                "INSERT INTO app_media_assets(id,install_id,title,kind,lifecycle,revision,creator,created,updated) \
                 VALUES('med-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',?1,'Legacy','image','draft',1,'{\"kind\":\"operator\"}',1.0,1.0)",
                rusqlite::params![fx.install],
            )
        })
        .unwrap();
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(
            &action,
            &token,
            &key,
            "med-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert!(
        body.is_empty(),
        "unlinked legacy asset carried bytes: {frame}"
    );
    assert!(
        frame.get("error").is_some() || frame["result"].is_null(),
        "unlinked legacy asset must be an error frame: {frame}"
    );

    // Revoked approval at the current digest: the live install
    // re-proof refuses the original read the same mount minted under.
    let install = fx
        .op("app_workspace_show", json!({"install_id": fx.install}))
        .expect("install show");
    let digest = install["digest"]
        .as_str()
        .or_else(|| install["install"]["digest"].as_str())
        .expect("install digest")
        .to_string();
    fx.op(
        "app_local_install_revoke",
        json!({"install_id": fx.install, "digest": digest}),
    )
    .expect("revoke");
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert!(body.is_empty(), "revoked install carried bytes: {frame}");
    assert!(
        frame.get("error").is_some() || frame["result"].is_null(),
        "revoked install must be an error frame: {frame}"
    );

    assert_eq!(
        fx.artifact_gets(),
        before,
        "no refused request reached the provider artifact GET"
    );
}

/// Binding/credential drift: the recorded binding's connection is
/// revoked after the run approved, so the CURRENT material no longer
/// matches the frozen record — the original read must refuse before
/// any provider access; a historical receipt is never a credential.
#[test]
fn cad1365_original_transfer_refuses_drifted_binding_material() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let before = fx.artifact_gets();

    // Revoke the enrolled connection the frozen binding records,
    // through the real `connection_revoke` verb — the drift class a
    // rotated/replaced credential is: current material can no longer
    // prove the recorded binding/revision. The seed MUST succeed; a
    // soft skip would test nothing.
    let record = fx
        .shared
        .store
        .platform_credential(agenticos_external::PLATFORM, ACCOUNT)
        .expect("credential read")
        .expect("the enrolled gen1365 credential exists");
    let reply = fx
        .op(
            "connection_revoke",
            json!({"connection_id": record.connection_id}),
        )
        .expect("seed: revoking the enrolled connection must succeed");
    assert_eq!(reply["revoked"], true, "{reply}");
    assert!(
        fx.shared
            .store
            .platform_credential(agenticos_external::PLATFORM, ACCOUNT)
            .expect("credential read")
            .is_none(),
        "the revoked credential row is gone"
    );
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert!(
        body.is_empty(),
        "drifted credential material carried bytes: {frame}"
    );
    assert!(
        frame.get("error").is_some() || frame["result"].is_null(),
        "drifted material must be an error frame: {frame}"
    );
    assert_eq!(
        fx.artifact_gets(),
        before,
        "a drift refusal runs before any original GET"
    );
}

/// Provider integrity AT DOWNLOAD TIME: the producer has already
/// succeeded — the receipt, private origin and descriptor are all
/// valid — then the artifact GET itself serves a corrupted body:
/// same-length wrong bytes (digest mismatch), a truncated payload,
/// or a non-image Content-Type. The daemon's existing 10 MiB
/// fetch/integrity guards must refuse each: an ordinary error frame,
/// NO success header and NO body bytes — and the provider counter
/// proves each refusal came from the REAL artifact fetch/verify path
/// (exactly one new GET per case), not an authority gate. Baseline
/// RED: "Unknown method" with zero new GETs — the missing feature,
/// never a passing negative.
#[test]
fn cad1365_original_transfer_refuses_tampered_download_bodies() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);

    // Same length, wrong content: the descriptor digest cannot match.
    let mut corrupted = fx.original.clone();
    let mid = corrupted.len() / 2;
    corrupted[mid] ^= 0xff;
    corrupted[mid + 1] ^= 0x5a;
    // Truncated: half the declared bytes.
    let truncated = fx.original[..fx.original.len() / 2].to_vec();

    let cases: Vec<(Vec<u8>, String, &str)> = vec![
        (corrupted, "image/png".into(), "corrupted same-length body"),
        (truncated, "image/png".into(), "truncated body"),
        (
            fx.original.clone(),
            "text/plain".into(),
            "wrong Content-Type",
        ),
    ];
    for (bytes, mime, what) in cases {
        *fx.get_override.lock().unwrap() = Some((bytes, mime));
        let before = fx.artifact_gets();
        let (reader, line) = socket_call(
            &fx,
            Asserted::Operator,
            "app_media_original",
            params(&action, &token, &key, &fx.asset_id),
        )
        .expect("socket");
        let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
        assert!(body.is_empty(), "{what}: refusal carried bytes: {frame}");
        assert!(
            frame.get("error").is_some() || frame["result"].is_null(),
            "{what}: refusal must be an ordinary error frame: {frame}"
        );
        assert_eq!(
            fx.artifact_gets(),
            before + 1,
            "{what}: the refusal must come from the real artifact \
             fetch/integrity guard — exactly one new provider GET"
        );
    }
}

/// A different permitted actor is never owner-filtered: a verified
/// named PUBLIC session (`Auth::open_public` — the post-JWT primitive
/// `board_session_open` calls) mints its own real media.read mount on
/// the SAME workspace catalog and downloads the producer-operator's
/// asset byte-exact. Creator freezes `{kind:"user",id:sub}` — it is
/// attribution, not a partition. Baseline RED: "Unknown method" — the
/// missing feature, never a passing negative.
#[test]
fn cad1365_original_transfer_a_different_permitted_public_actor_reads() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    // The named public session is minted inside THIS daemon's own
    // `Auth` through the real `open_public` — a verified platform user,
    // never a faked or body-selected identity.
    let now = fx.shared.operator_now();
    let config = crate::board_identity::read_config(&fx.state).unwrap();
    let realm = crate::operator_auth::PublicRealm {
        issuer: config.issuer,
        company: config.company,
        host: config.host,
    };
    let public_token = fx
        .shared
        .operator_auth()
        .open_public_bound(
            crate::operator_auth::BoardUser {
                sub: "usr-collab-reader".to_string(),
                email: "collab@example.test".to_string(),
                name: "Collab Reader".to_string(),
                role: "operator".to_string(),
                handle: "collab".to_string(),
            },
            &format!("jti-collab-{}", std::process::id()),
            now + 60,
            "cad1365-acceptance",
            realm,
            now,
        )
        .unwrap()
        .expect("fresh jti opens a session")
        .token;
    // Mint + consume the media mount against the public session —
    // `Origin::Public` and its empty page key are the real credential
    // grammar (`key` is absent on the public cookie-only surface).
    let mint = fx
        .op(
            "app_screen_mint",
            json!({"install_id": fx.install, "context_id": fx.context,
                "tag": "media", "token": public_token, "key": "",
                "origin": "public", "generation": 1}),
        )
        .expect("public mint must succeed — the actor is permitted");
    let nonce = mint["mount"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    fx.op("app_screen_consume", json!({"nonce": nonce}))
        .expect("public consume must succeed");
    let action = mint["action_token"].as_str().unwrap().to_string();

    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        json!({"action_token": action, "token": public_token, "key": "",
            "origin": "public", "asset_id": fx.asset_id}),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert_eq!(
        frame["result"]["transfer"], "app-media-original/1",
        "a permitted public actor must receive the header: {frame}"
    );
    assert_eq!(
        body, fx.original,
        "the collaborator reads the producer's original byte-exact — \
         creator is attribution, never a partition"
    );
}

/// HTTP strictness: the board's closed body refuses forged session/ref/
/// scope selectors BEFORE dispatch (400), agent/unproven callers stop
/// at the write gate (403), and an operator request carries only
/// `{action_token, asset_id}` — its real session does the proving.
/// Baseline: 404 on the missing route is the honest RED.
#[test]
fn cad1365_original_transfer_http_body_and_caller_gates() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let (port, _board_thread) = board(&fx);
    let seam = Seam::token_at(&fx.state).unwrap();
    let before = fx.artifact_gets();

    for who in ["agent:worker", "unproven"] {
        let (status, _c, _d, body) = http_original(
            port,
            &seam,
            who,
            None,
            json!({"action_token": action, "asset_id": fx.asset_id}),
        );
        assert_eq!(status, 403, "{who} reached the original route: {body:?}");
    }

    // Closed body: caller-chosen credential/provenance/scope fields
    // refuse before the daemon ever sees the request.
    for field in [
        "token",
        "key",
        "origin",
        "provider_ref",
        "artifact_ref",
        "url",
        "digest",
        "sha256",
        "bytes",
        "workspace",
        "creator",
        "caller",
        "connection_id",
        "install_id",
        "context_id",
        "account",
    ] {
        let (status, _c, _d, body) = http_original(
            port,
            &seam,
            "operator",
            Some((&token, &key)),
            json!({"action_token": action, "asset_id": fx.asset_id,
                field: "forged"}),
        );
        assert_eq!(
            status, 400,
            "body field {field} rode the original request: {body:?}"
        );
    }

    // A forged action token under the real session relays to a daemon
    // refusal — the HTTP peer is at least as strict as the RPC.
    let (status, _c, _d, body) = http_original(
        port,
        &seam,
        "operator",
        Some((&token, &key)),
        json!({"action_token": "d".repeat(64), "asset_id": fx.asset_id}),
    );
    assert!(status >= 400, "forged action token over HTTP: {body:?}");

    assert_eq!(
        fx.artifact_gets(),
        before,
        "HTTP refusals run before any original GET"
    );
}

/// AUTHORITY LIFETIME — the ONE essential bad case the transport
/// contract pins: a VALID authenticated native `app_media_original`
/// begins (the real header already came back through
/// `serve::handle_conn`, proving initial authority and a fetched
/// artifact), then the session OR the mount is revoked while the
/// client is paused mid-body. Already-queued bytes cannot be recalled
/// — a partial queued prefix is allowed — but the daemon MUST NOT
/// finish delivering the complete original after revocation; a native
/// client comparing the declared `bytes` to what arrived refuses a
/// truncated stream, so `body.len() < declared` is the honest
/// refusal. Session and mount revocation are two axes of the SAME
/// lifetime requirement, tabled in one test: each axis re-boots its
/// own fixture so revocation never carries over. The reader's 5s
/// bound is a fixture constraint on how long a kernel-buffered stream
/// may take to drain — no sleep, pacing or producer cooperation.
/// Baseline RED: a greenfield head either answers `Unknown method`
/// (no header — caught by the positive assertions) or streams all
/// `declared` bytes anyway — authority checked once at dispatch, never
/// during the write loop.
#[test]
fn cad1365_original_transfer_authority_lifetime() {
    for axis in ["session", "mount"] {
        let fx = Fx::boot(noisy_square_png(), None, true);
        let (token, key) = fx.session();
        let (_nonce, action) = fx.mount(&token, &key, 1);
        let before = fx.artifact_gets();

        // Positive first: a REAL `app_media_original` request through
        // `serve::handle_conn`. `socket_call` returns the BufReader
        // with the reply line read — any kernel-buffered body prefix
        // the daemon already wrote stays queued in the stream.
        let (reader, line) = socket_call(
            &fx,
            Asserted::Operator,
            "app_media_original",
            params(&action, &token, &key, &fx.asset_id),
        )
        .expect("original request reaches the daemon");
        let frame: Value = serde_json::from_str(&line)
            .unwrap_or_else(|_| panic!("reply line is not JSON: {line}"));
        let result = frame.get("result").cloned().unwrap_or(Value::Null);
        assert_ne!(
            result,
            Value::Null,
            "{axis}: the authority-lifetime case needs a real success header \
             first — an up-front refusal tests nothing: {frame}"
        );
        assert_eq!(
            result["transfer"], "app-media-original/1",
            "{axis}: initial authority must pass: {frame}"
        );
        let declared = result["bytes"].as_u64().expect("header bytes") as usize;
        assert_eq!(
            declared,
            fx.original.len(),
            "{axis}: declared bytes are the fetched original: {frame}"
        );
        assert!(declared > 2 * 1024 * 1024, "{axis}: >2 MiB original");
        assert_eq!(
            fx.artifact_gets(),
            before + 1,
            "{axis}: the header proves the real provider artifact GET ran"
        );

        // The client pauses: the >2 MiB body cannot fit one unix
        // socket send buffer (~212 KiB default), so the daemon's
        // writer is still blocked in the transfer while we revoke.
        match axis {
            "session" => {
                // The REAL `Auth` this daemon's sessions live in —
                // the same `operator_auth()` `fx.session()` minted
                // through — revoking the actual session token.
                assert!(
                    fx.shared
                        .operator_auth()
                        .revoke_token(&token)
                        .expect("session revoke"),
                    "{axis}: the live session must actually revoke"
                );
            }
            _ => {
                // The REAL `app_tool_revoke` verb — its owner proof
                // (`SameSession`: the proven session owns the mount's
                // context) then `revoke_tool_context`.
                let reply = fx
                    .op(
                        "app_tool_revoke",
                        json!({"action_token": action, "token": token,
                            "key": key, "origin": "loopback"}),
                    )
                    .expect("mount revoke");
                assert_eq!(
                    reply["revoked"], true,
                    "{axis}: the live mount must actually revoke: {reply}"
                );
            }
        }

        // Drain what the daemon actually delivers post-revoke — the
        // same BufReader, bounded at the ORIGINAL_LIMIT contract cap.
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("fixture read bound");
        let mut body = Vec::new();
        reader
            .take(ORIGINAL_LIMIT as u64 + 1)
            .read_to_end(&mut body)
            .expect("revocation must close the original stream, not stall or time out");
        assert!(
            body.len() < declared,
            "{axis}: the daemon completed a {declared}-byte original AFTER \
             the {axis} was revoked — {} bytes arrived",
            body.len()
        );
    }
}

/// SLOW INSTALL REVOKE (bot finding on ac18, Major): the original
/// stream's authority is the install approval, not only session/mount.
/// POSITIVE first: a REAL >2 MiB original header succeeds while its
/// body stays UNDRAINED, so the daemon's writer is paused on the
/// kernel socket buffer. The REAL `app_local_install_revoke` then runs
/// concurrently and must complete within a generous 2 s bound. The
/// original stream must then truncate: the daemon must not finish the
/// declared bytes after the install authority is revoked. A stalled
/// revoke closes the client first, so the blocked writer releases its
/// locks, joins its own thread, and only then fails.
#[test]
fn cad1365_original_transfer_install_revoke_closes_a_paused_stream() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let before = fx.artifact_gets();

    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("original request reaches the daemon");
    let frame: Value =
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("reply line is not JSON: {line}"));
    let result = frame.get("result").cloned().unwrap_or(Value::Null);
    assert_ne!(
        result,
        Value::Null,
        "install revoke needs a real success header first — an up-front refusal tests nothing: {frame}"
    );
    assert_eq!(
        result["transfer"], "app-media-original/1",
        "initial authority must pass: {frame}"
    );
    let declared = result["bytes"].as_u64().expect("header bytes") as usize;
    assert_eq!(
        declared,
        fx.original.len(),
        "declared bytes are the fetched original: {frame}"
    );
    assert!(declared > 2 * 1024 * 1024, ">2 MiB original");
    assert_eq!(
        fx.artifact_gets(),
        before + 1,
        "the header proves the real provider artifact GET ran"
    );

    let install = fx
        .op("app_workspace_show", json!({"install_id": fx.install}))
        .expect("install show");
    let digest = install["digest"]
        .as_str()
        .or_else(|| install["install"]["digest"].as_str())
        .expect("install digest")
        .to_string();
    assert_eq!(
        fx.shared
            .store
            .app_capability_status(&fx.install, &digest)
            .unwrap()["state"],
        "approved",
        "the install is approved before the revoke"
    );

    // The REAL revoke, on its own thread and its own assertion scope,
    // while the client body is still undrained.
    let (done, revoke_reply) = std::sync::mpsc::channel();
    let (state, install_id) = (fx.state.clone(), fx.install.clone());
    let revoke_digest = digest.clone();
    let revoker = std::thread::spawn(move || {
        let reply = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "app_local_install_revoke",
                json!({"install_id": install_id, "digest": revoke_digest}),
            )
        });
        let _ = done.send(reply);
    });

    match revoke_reply.recv_timeout(Duration::from_secs(2)) {
        Ok(reply) => {
            revoker.join().expect("revoke thread");
            let reply = reply.expect("operator local-install revoke must succeed");
            assert_eq!(
                reply["approved"], false,
                "the reply reports the revoke: {reply}"
            );
            assert_eq!(
                reply["digest"], digest,
                "the reply names the revoked digest"
            );
            assert_eq!(
                fx.shared
                    .store
                    .app_capability_status(&fx.install, &digest)
                    .unwrap()["state"],
                "revoked",
                "the store records the install as revoked, not approved"
            );
        }
        Err(_) => {
            // Close the client side so the paused writer fails and
            // releases the locks; then join our own thread and fail.
            drop(reader);
            revoker.join().expect("revoke thread");
            panic!(
                "app_local_install_revoke did not complete within 2 s while the \
                 original body was undrained — it waited on the paused writer"
            );
        }
    }

    // Drain what the daemon delivers after the install revoke, bounded
    // at the contract cap; a truncated stream must stop short of the
    // declared length.
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("fixture read bound");
    let mut body = Vec::new();
    reader
        .take(ORIGINAL_LIMIT as u64 + 1)
        .read_to_end(&mut body)
        .expect("revocation must close the original stream, not stall or time out");
    assert!(
        body.len() < declared,
        "the daemon completed a {declared}-byte original AFTER the install was revoked — {} bytes arrived",
        body.len()
    );
}

/// CROSS-INSTALL READ UNDER A CRASHED LEGACY MARKER (CAD-1365 c63
/// spec/security Finding A). A second, permitted generic media consumer
/// (distinct app id, its own mount and context) reads the producer's
/// original while the legacy seqlock is left ODD in the real coordination
/// file `<pm>/.git/cadence-legacy-generation`, as a crashed legacy writer
/// would. Positive first: the held snapshot must still serve the
/// byte-exact original with exactly one provider GET and must not repair
/// the marker. Bad case under the same odd marker: after the producer's
/// approval is revoked through the real `app_local_install_revoke`, the
/// cross-install read refuses with no header, no body and no provider GET.
/// Baseline: RED at the positive — the odd marker surfaces as busy, not a
/// setup failure.
#[test]
fn cad1365_original_transfer_cross_install_reads_under_a_crashed_legacy_marker() {
    let fx = Fx::boot(noisy_square_png(), None, true);
    let app = fx._dir.path().join("consumer");
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(
        app.join("app.md"),
        APP_MD
            .replace("designer-fixture", "designer-consumer")
            .replace("# Designer fixture", "# Designer consumer"),
    )
    .unwrap();
    std::fs::write(app.join("workflows/create-image.md"), WORKFLOW_MD).unwrap();
    media_screen(&app);
    let screens = app.join("screens/media/screens.json");
    let mut decl: Value =
        serde_json::from_str(&std::fs::read_to_string(&screens).unwrap()).unwrap();
    decl["app"] = json!("designer-consumer");
    std::fs::write(&screens, serde_json::to_string_pretty(&decl).unwrap()).unwrap();
    let installed = fx
        .op(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        )
        .expect("consumer install");
    let consumer = installed["install_id"].as_str().unwrap().to_string();
    fx.op(
        "app_local_install_approve",
        json!({"install_id": consumer, "digest": installed["digest"]}),
    )
    .expect("consumer approve");
    fx.op(
        "app_binding_create",
        json!({"install_id": consumer, "slot": "image",
            "connection_id": "gen1365", "request_id": "bind-1365-consumer"}),
    )
    .expect("consumer bind");
    let ctx = fx
        .op(
            "app_context_create",
            json!({"install_id": consumer, "label": "media",
                "input_defaults": {}, "request_id": "ctx-1365-consumer"}),
        )
        .expect("consumer context");
    let context = ctx["context"]["id"]
        .as_str()
        .or_else(|| ctx["id"].as_str())
        .unwrap()
        .to_string();
    let (token, key) = fx.session();
    let mint = fx
        .op(
            "app_screen_mint",
            json!({"install_id": consumer, "context_id": context,
                "tag": "media", "token": token, "key": key,
                "origin": "loopback", "generation": 1}),
        )
        .expect("consumer mint");
    let nonce = mint["mount"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();
    fx.op("app_screen_consume", json!({"nonce": nonce}))
        .expect("consumer consume");
    let action = mint["action_token"].as_str().unwrap().to_string();

    let marker = fx
        .state
        .parent()
        .unwrap()
        .join("pm/.git/cadence-legacy-generation");
    std::fs::write(&marker, "7").unwrap();
    let before = fx.artifact_gets();
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert_eq!(
        frame["result"]["transfer"], "app-media-original/1",
        "a permitted cross-install consumer must read the producer's original under a crashed legacy marker: {frame}"
    );
    assert_eq!(
        body, fx.original,
        "the cross-install original is byte-exact under the held snapshot"
    );
    assert_eq!(
        fx.artifact_gets(),
        before + 1,
        "the positive read ran exactly one provider artifact GET"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "7",
        "the read must not repair the crashed legacy marker"
    );

    let install = fx
        .op("app_workspace_show", json!({"install_id": fx.install}))
        .expect("producer show");
    let digest = install["digest"]
        .as_str()
        .or_else(|| install["install"]["digest"].as_str())
        .expect("producer digest")
        .to_string();
    assert_eq!(
        fx.shared
            .store
            .app_capability_status(&fx.install, &digest)
            .unwrap()["state"],
        "approved",
        "the producer is approved before the revoke"
    );
    let reply = fx
        .op(
            "app_local_install_revoke",
            json!({"install_id": fx.install, "digest": digest}),
        )
        .expect("producer revoke");
    assert_eq!(
        reply["approved"], false,
        "the reply reports the revoke: {reply}"
    );
    assert_eq!(
        reply["digest"], digest,
        "the reply names the revoked digest"
    );
    assert_eq!(
        fx.shared
            .store
            .app_capability_status(&fx.install, &digest)
            .unwrap()["state"],
        "revoked",
        "the store records the producer as revoked"
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "7",
        "the revoke must leave the crashed marker unchanged"
    );
    let before = fx.artifact_gets();
    let (reader, line) = socket_call(
        &fx,
        Asserted::Operator,
        "app_media_original",
        params(&action, &token, &key, &fx.asset_id),
    )
    .expect("socket");
    let (frame, body) = read_transfer(reader, line, ORIGINAL_LIMIT);
    assert!(
        body.is_empty(),
        "a revoked producer carried bytes under the crashed marker: {frame}"
    );
    assert!(
        frame.get("error").is_some() || frame["result"].is_null(),
        "a revoked producer must be an error frame under the crashed marker: {frame}"
    );
    assert_eq!(
        fx.artifact_gets(),
        before,
        "no refused cross-install request reached the provider artifact GET"
    );
}
