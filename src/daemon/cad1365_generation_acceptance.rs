//! Independent CAD-1365 governed generation acceptance, written by the
//! acceptance author from the ticket and
//! `design-plans/designer-generation-provenance-v1.md` — NOT by the
//! pipeline implementers, who may not edit or weaken it.
//!
//! What this proves end to end on ONE live native `Shared`: the declared
//! host pipeline `app_run_create` → `app_run_approve` →
//! `app_run_dispatch` (native RPC dispatch as the real asserted operator
//! connection) on a Designer-like installed host-execution `create-image`
//! workflow → frozen `media.generate v1` binding resolved through the real
//! `connection_list_locked`/`app_binding_config` path → the real media
//! price read frozen into the snapshot at create and re-quoted fresh at
//! dispatch (CAD-1141: the fresh quote is the ceiling) → approve-by-digest → host dispatch → the REAL
//! `AgenticosExternalAdapter` talking the real submit/poll/artifact
//! protocol to a loopback provider fixture → verified original
//! descriptor/bytes before the custody derivative →
//! `app_capability_record` → ONE durable image receipt + ONE draft
//! `app_media_assets` catalog row + ONE private
//! `app_media_asset_origins` link, with `creator` frozen from the
//! verified session inside the snapshot before its digest.
//!
//! `app_run_start` (the single-call create+approve+dispatch for agent
//! workflows) is ABI-refused for host-execution workflows on this head:
//! it resolves the installation's default team and freezes
//! `owner_pm`, which a host run's snapshot contract refuses
//! ("a host-execution run has no owner PM"). The declared host path is
//! the three-verb operator click chain — same snapshot digest approval
//! and the same fresh re-quote at dispatch — so this
//! acceptance rides it and never weakens the team or owner rules.
//!
//! Fixture honesty: the provider is a loopback HTTP server answering the
//! EXISTING AgenticOS media schema (price read, submit, job read,
//! artifact bytes) — never a new API, fake SDK or in-process stub. The
//! daemon's own adapter issues every request; the test never touches its
//! internals. Custody is the file backend (`Custody::file`) inside the fixture tempdir with
//! a synthetic token; no real credential, keychain, paid provider or
//! network beyond loopback is involved. The operator session is minted
//! through the REAL `operator_link_mint`/`operator_session_open` RPCs on
//! the same `Shared` (`Auth::load` is in-memory, so the row must live in
//! this daemon). No socket/HTTP board here — mount/HTTP authority is
//! separately covered by the 18 existing native checks. Private
//! `platform_custody` is set before the `Arc` escapes — the exact
//! pattern `app_capabilities_rpc::tests` already uses; no new field,
//! seam or API. The enrolled connection row is written once through a
//! store-only seed (`Shared::new` leaves it unused until after the seed
//! — the same ordering `platform_enroll` rows get in the frozen fixture
//! modules), never a fake auth or grant bypass.
//!
//! Honest RED/GREEN: if the declared host pipeline already works this
//! passes unchanged; if it fails, the refusal text is the real missing
//! feature, never a fixture ABI error.

#![cfg(all(test, feature = "test-seam"))]

#[path = "cad1258_lock2_refusal_acceptance.rs"]
mod cad1258_lock2_refusal_acceptance;
#[path = "cad1258_worker_tests.rs"]
mod cad1258_worker_tests;

use super::*;
use crate::platform::{agenticos_external, Custody, Key};
use crate::store::StoreConn;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The Designer-like installed package: one host-execution
/// `create-image` workflow declaring the `image` capability slot
/// (`media.generate v1`, draft, connection_account) whose step action is
/// the reviewed `local.capability.call`. Frozen manual
/// subject/source/image_prompt inputs ride the snapshot —
/// `authority.source` stays JSON-null, which `image_prompt` admits for
/// factual (non-URL) manual sources. No Social receipt or draft exists
/// on this path.
const APP_MD: &str = r#"---
app: designer-fixture
title: Designer fixture
version: '1.0.0'
summary: One host-execution image-generation workflow for CAD-1365 acceptance.
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

/// A valid AGENT workflow beside the host one: proves `app_run_start`'s
/// installation-team floor is a real guard, not an incidental workflow
/// refusal — the start must stop at `team_of` before any worker
/// resolution or provider work. `writer` is a declared input the
/// installation team would fill; without a team the call cannot reach
/// it.
const TEAM_WORKFLOW_MD: &str = r#"---
title: "Team note: {{topic}}"
goal: "One short note about the topic"
label: Team note
inputs:
  topic: { ask: "Topic", example: "Release note" }
  writer: { ask: "Registered writer in the owner PM group" }
---

One short text note.

## Draft the note: {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Write one short plain-text note about the topic.

### Acceptance
- [ ] one short plain-text note
"#;

const JOB_ID: &str = "med_job1365gen";
const CHARGE: u64 = 31_500;

/// A small valid in-custody PNG — the provider original AND derivative.
fn png_1x1() -> Vec<u8> {
    use image::ImageEncoder as _;
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[7u8], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    bytes
}

#[derive(Debug)]
struct DoorRecord {
    method: String,
    url: String,
    body: Value,
}

/// Loopback AgenticOS media door: the EXISTING provider schema — price
/// read, submit, job poll, artifact bytes — joined before the tempdir
/// drops. `charge` lets a case move the price without touching the
/// door shape.
fn media_door(
    charge: Arc<std::sync::atomic::AtomicU64>,
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
    let original = png_1x1();
    let digest = format!("{:x}", Sha256::digest(&original));
    let worker = std::thread::spawn(move || {
        // Serve until the fixture drops: `stop` is checked every poll
        // round, so teardown costs one recv_timeout instead of a fixed
        // lifetime. No fixed-duration wait — the door ends when the test
        // is done, not on a clock.
        while !stop.load(Ordering::SeqCst) {
            let mut request = match server.recv_timeout(Duration::from_millis(100)) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(_) => break,
            };
            let mut text = String::new();
            request.as_reader().read_to_string(&mut text).unwrap();
            let (method, url) = (request.method().to_string(), request.url().to_string());
            let charge_minor = charge.load(std::sync::atomic::Ordering::SeqCst);
            let reply = match (method.as_str(), url.as_str()) {
                ("GET", "/v1/runtime/media/price/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5",
                            "price":{"slug":"generate_image","chargeMinor":charge_minor,
                            "currency":"USD","version":"2026-09-29T00:00:00.000Z"}}})
                    .to_string(),
                )
                .with_status_code(200),
                ("POST", "/v1/runtime/media/image") => tiny_http::Response::from_string(
                    json!({"ok":true,"data":{"job":{
                            "id":JOB_ID,"kind":"image","status":"succeeded",
                            "model":"openai/gpt-image-2.5","provider":"upstream-fixture",
                            "providerTaskId":null,
                            "artifacts":[{"ref":format!("{JOB_ID}.{digest}"),
                                "digest":digest,"bytes":original.len(),"mime":"image/png"}],
                            "artifactError":null,"usage":{"providerCredits":null},
                            "price":{"slug":"generate_image","chargeMinor":charge_minor,
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
                    tiny_http::Response::from_data(original.clone()).with_header(
                        tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                    )
                }
                (m, u) => panic!("unexpected provider request {m} {u}"),
            };
            seen_worker.lock().unwrap().push(DoorRecord {
                method,
                url,
                body: serde_json::from_str(&text).unwrap_or(Value::Null),
            });
            request.respond(reply).unwrap();
        }
    });
    (base, seen, worker)
}

/// The fixture daemon: `Shared::new` on its own temp state/pm dirs, file
/// custody (never the host keychain), a REAL AgenticOS adapter pinned to
/// the loopback provider, and an enrolled synthetic connection whose
/// reviewed registration matches its own adapter's sink registration.
struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
    install: String,
    seen: Arc<Mutex<Vec<DoorRecord>>>,
    charge: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1365gen")
            .tempdir()
            .unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let charge = Arc::new(std::sync::atomic::AtomicU64::new(CHARGE));
        let stop = Arc::new(AtomicBool::new(false));
        let (base, seen, worker) = media_door(charge.clone(), stop.clone());
        let mut opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        // Fast native media polling inside THIS daemon's environment;
        // the submit already returns a succeeded job, so no poll runs.
        opts.provider_env.set("CADENCE_TEST_MEDIA_POLL_MS", "20");
        opts.provider_env
            .set("CADENCE_TEST_MEDIA_DEADLINE_MS", "5000");
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
        let mut shared = Shared::new(dir.path(), &opts).unwrap();
        // No installation team: `app_run_start` derives owner from the
        // install's default team, and a host-execution run forbids an
        // owner PM — the unset team is the honest fixture shape.
        // Synthetic file custody only; never consult an installed keychain.
        // The private field is set before the Arc escapes, through the
        // crate-private `Shared` — the exact pattern the existing
        // `app_capabilities_rpc::tests` fixture uses.
        Arc::get_mut(&mut shared).unwrap().platform_custody =
            Custody::file(dir.path().join("fixture-custody"));
        // A fixture-generated synthetic token under the exact reviewed
        // account shape — never a real credential.
        let token = b"synthetic-cad1365-gen-token";
        const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";
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
        let app = dir.path().join("app");
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(app.join("app.md"), APP_MD).unwrap();
        std::fs::write(app.join("workflows/create-image.md"), WORKFLOW_MD).unwrap();
        std::fs::write(app.join("workflows/team-note.md"), TEAM_WORKFLOW_MD).unwrap();
        let op = |method: &str, params: Value| -> Result<Value> {
            scoped(Asserted::Operator, || {
                shared.dispatch(method, &params, std::process::id())
            })
        };
        let installed = op(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        )
        .expect("install");
        let install = installed["install_id"].as_str().unwrap().to_string();
        op(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        )
        .expect("approve");
        op(
            "app_binding_create",
            json!({"install_id": install, "slot": "image",
                "connection_id": "gen1365", "request_id": "bind-1365"}),
        )
        .expect("the image slot must bind through the real declare path");
        Self {
            shared,
            install,
            seen,
            charge,
            stop,
            worker: Some(worker),
            _dir: dir,
        }
    }

    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        scoped(Asserted::Operator, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    /// A real operator session through the native link mint + open on
    /// THIS daemon's own `Auth` (the session row is in-memory, so the
    /// request's `tool_session` resolves it natively).
    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.shared.state_dir).unwrap();
        let secret = crate::operator_auth::read_secret(&self.shared.state_dir).unwrap();
        let nonce = self
            .operator(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap()["nonce"]
            .clone();
        let opened = self
            .operator(
                "operator_session_open",
                json!({"nonce": nonce, "origin": "loopback"}),
            )
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_string(),
            opened["key"].as_str().unwrap().to_string(),
        )
    }

    /// The native quote the host would draw for the `image` slot —
    /// the real `app_binding_quote` RPC over the loopback price read,
    /// never a fabricated price.
    fn quote(&self) -> Value {
        self.operator(
            "app_binding_quote",
            json!({"install_id": self.install, "slot": "image"}),
        )
        .expect("the bound image slot must quote against the loopback price read")["quote"]
            .clone()
    }

    /// A real `app_run_start` call — the single-click entry the app's
    /// `planRunStart` issues — with the relayed native operator session
    /// credential fields.
    fn start(
        &self,
        workflow: &str,
        inputs: Value,
        expected: Value,
        request_id: &str,
    ) -> Result<Value> {
        let (token, key) = self.session();
        self.operator(
            "app_run_start",
            json!({"install_id": self.install, "workflow": workflow,
                "inputs": inputs,
                "request_id": request_id, "expected_quotes": expected,
                "token": token, "key": key, "origin": "loopback"}),
        )
    }

    /// The declared host pipeline: create (freezing the real price
    /// read into the snapshot), approve by snapshot digest, dispatch.
    /// `request_id` distinguishes runs; the session rides the real
    /// relayed credential fields.
    fn create(&self, request_id: &str) -> Result<Value> {
        let (token, key) = self.session();
        self.operator(
            "app_run_create",
            json!({"install_id": self.install, "workflow": "create-image",
                "inputs": {"subject": "Customer follow-up",
                    "source": "Acme CRM helps small teams track customers",
                    "image_prompt": "Use a calm editorial palette"},
                "request_id": request_id,
                "token": token, "key": key, "origin": "loopback"}),
        )
    }

    fn approve_dispatch(&self, run: &Value) -> Result<Value> {
        let id = run["id"].as_str().unwrap();
        self.operator(
            "app_run_approve",
            json!({"run_id": id, "digest": run["snapshot_digest"]}),
        )?;
        self.operator("app_run_dispatch", json!({"run_id": id}))
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The declared host pipeline end to end: a positive frozen quote starts
/// the run, the REAL adapter submits/polls/fetches the loopback job, the
/// verified original becomes the retained derivative, the run succeeds,
/// and ONE draft catalog asset links to the ONE immutable receipt with
/// the operator creator frozen in the snapshot.
#[test]
fn cad1365_generation_declared_host_run_retains_receipt_catalog_and_creator() {
    let fx = Fx::new();
    let created = fx
        .create("run-1365-gen")
        .expect("run creation must freeze the real quote");
    assert_eq!(created["state"], "awaiting_approval", "{created}");
    let run = fx
        .approve_dispatch(&created)
        .expect("declared host run must complete");
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(run["snapshot"]["creator"], json!({"kind": "operator"}));

    // The real adapter traffic: price read(s), ONE submit under the
    // deterministic call id, then the artifact GET — never a re-POST.
    let seen = fx.seen.lock().unwrap();
    let submits: Vec<&DoorRecord> = seen
        .iter()
        .filter(|r| r.method == "POST" && r.url == "/v1/runtime/media/image")
        .collect();
    assert_eq!(submits.len(), 1, "exactly one image submit: {seen:?}");
    assert_eq!(submits[0].body["model"], "openai/gpt-image-2.5");
    assert_eq!(submits[0].body["aspectRatio"], "1:1");
    assert!(
        submits[0].body["prompt"]
            .as_str()
            .is_some_and(|p| p.contains("Acme CRM helps small teams track customers")
                && p.contains("Use a calm editorial palette")),
        "frozen prompt rode the submit body: {}",
        submits[0].body
    );
    assert!(
        seen.iter()
            .any(|r| r.method == "GET" && r.url.starts_with("/v1/runtime/media/artifacts/")),
        "the retained original was fetched: {seen:?}"
    );
    drop(seen);

    // ONE durable image receipt with the verified original descriptor.
    let receipts = fx
        .operator("app_run_capability_results", json!({"run_id": run["id"]}))
        .expect("capability receipts");
    let rows = receipts["results"]
        .as_array()
        .expect("results is an array of receipts");
    assert_eq!(rows.len(), 1, "one retained capability receipt: {receipts}");
    let receipt = &rows[0];
    assert_eq!(receipt["result"]["kind"], "media.generated.image");
    assert_eq!(
        receipt["result"]["asset_sha256"],
        format!("sha256:{:x}", Sha256::digest(png_1x1()))
    );
    assert_eq!(
        receipt["result"]["original_artifact"]["bytes"],
        png_1x1().len() as u64
    );
    assert_eq!(
        receipt["result"]["original_artifact"]["sha256"],
        format!("sha256:{:x}", Sha256::digest(png_1x1()))
    );

    // ONE draft catalog asset with the operator creator, linked privately
    // to the immutable receipt. `conn()` is store-private; the private
    // association is read inside `write_tx` through `StoreConn` — the
    // same lane the publish acceptance reads its persisted row on.
    let (page, _next) = fx.shared.store.app_media_catalog(20, None).unwrap();
    assert_eq!(page.len(), 1, "one catalog asset");
    let asset = &page[0];
    assert_eq!(asset.title, "Generated image");
    assert_eq!(asset.kind, "image");
    assert_eq!(asset.lifecycle, "draft");
    assert_eq!(asset.projection()["creator"], json!({"kind": "operator"}));
    let linked: Option<String> = fx
        .shared
        .store
        .write_tx(|tx| {
            Ok(tx.query_opt(
                "SELECT receipt_id FROM app_media_asset_origins WHERE asset_id=?1",
                rusqlite::params![asset.id],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(
        linked.as_deref(),
        receipt["id"].as_str(),
        "the private origin row must link the asset to its receipt"
    );
}

/// CAD-1141 (operator decision 2026-10-26, "Proceed within bound"): a
/// provider price that moves after the run froze its quote no longer
/// refuses. The fresh re-quote at dispatch proceeds, the image submit
/// carries the NEW quote as its `max_charge_minor` ceiling, and the
/// frozen, digest-covered snapshot still records the approved quote.
#[test]
fn cad1365_generation_quote_drift_proceeds_with_the_fresh_quote_as_the_ceiling() {
    let fx = Fx::new();
    let created = fx
        .create("run-1365-stale")
        .expect("run creation must freeze the real quote");
    assert_eq!(
        created["snapshot"]["quotes"]["image"]["total_price_micros"],
        CHARGE
    );
    let drifted = CHARGE * 2;
    fx.charge.store(drifted, Ordering::SeqCst);
    let run = fx
        .approve_dispatch(&created)
        .expect("a drifted price must not refuse the run");
    assert_eq!(run["state"], "succeeded", "{run}");
    let seen = fx.seen.lock().unwrap();
    let submits: Vec<&DoorRecord> = seen
        .iter()
        .filter(|r| r.method == "POST" && r.url == "/v1/runtime/media/image")
        .collect();
    assert_eq!(submits.len(), 1, "exactly one image submit: {seen:?}");
    assert_eq!(
        submits[0].body["max_charge_minor"], drifted,
        "the ceiling sent must be the fresh quote: {}",
        submits[0].body
    );
    drop(seen);
    let receipts = fx
        .operator("app_run_capability_results", json!({"run_id": run["id"]}))
        .expect("capability receipts");
    let rows = receipts["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one retained capability receipt: {receipts}");
    assert_eq!(rows[0]["result"]["quoted_micros"], drifted, "{receipts}");
    assert_eq!(
        run["snapshot"]["quotes"]["image"]["total_price_micros"], CHARGE,
        "the frozen snapshot is never rewritten"
    );
    assert_eq!(run["snapshot_digest"], created["snapshot_digest"]);
}

/// A fresh quote beyond the supported price bound (1e9 micros) still
/// refuses BEFORE the image submit: no job is posted and no receipt,
/// catalog asset or origin row exists. The quote GET itself is
/// legitimate; only the paid submit is gated.
#[test]
fn cad1365_generation_quote_beyond_bound_refuses_before_submit_and_leaves_nothing() {
    let fx = Fx::new();
    let created = fx
        .create("run-1365-bound")
        .expect("run creation must freeze the real quote");
    fx.charge.store(1_000_000_001, Ordering::SeqCst);
    let refused = fx
        .approve_dispatch(&created)
        .expect_err("a quote beyond the supported bound must be refused");
    assert!(
        refused
            .to_string()
            .contains("bound capability price discovery refused"),
        "wrong guard: {refused}"
    );
    let seen = fx.seen.lock().unwrap();
    assert!(
        !seen
            .iter()
            .any(|r| r.method == "POST" && r.url == "/v1/runtime/media/image"),
        "no image submit may leave before the quote refusal: {seen:?}"
    );
    drop(seen);
    let receipts = fx
        .operator(
            "app_run_capability_results",
            json!({"run_id": created["id"]}),
        )
        .expect("capability receipts");
    assert_eq!(
        receipts["results"].as_array().map_or(0, Vec::len),
        0,
        "no receipt on a refused run: {receipts}"
    );
    let (page, _next) = fx.shared.store.app_media_catalog(20, None).unwrap();
    assert!(page.is_empty(), "no catalog asset on a refused run");
    let origins: i64 = fx
        .shared
        .store
        .write_tx(|tx| {
            Ok(
                tx.query_row("SELECT count(*) FROM app_media_asset_origins", [], |r| {
                    r.get(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(origins, 0, "no origin row on a refused run");
}

/// THE GAP THE TICKET NAMES: `app_run_start` — the single-click entry
/// the app's host button (`planRunStart`) actually calls — must drive
/// the SAME declared host pipeline to `succeeded` on this fixture: the
/// frozen `expected_quotes` equals the native `app_binding_quote` answer,
/// the run approves and dispatches itself, the real adapter submits ONE
/// image job, and the receipt/catalog/creator/origin land exactly as the
/// split-path control proves. `start_app_run` unconditionally requires
/// an installation default team today, and `create_app_run` freezes the
/// team-derived `owner_pm` a host snapshot forbids — the honest RED on
/// this head is the missing-default-team refusal, never a skipped run.
#[test]
fn cad1365_generation_app_run_start_executes_the_declared_host_run() {
    let fx = Fx::new();
    let expected = json!({"image": fx.quote()});
    let run = fx
        .start(
            "create-image",
            json!({"subject": "Customer follow-up",
                "source": "Acme CRM helps small teams track customers",
                "image_prompt": "Use a calm editorial palette"}),
            expected,
            "run-1365-start",
        )
        .expect("app_run_start must drive the declared host run");
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(run["snapshot"]["creator"], json!({"kind": "operator"}));
    let seen = fx.seen.lock().unwrap();
    let submits: Vec<&DoorRecord> = seen
        .iter()
        .filter(|r| r.method == "POST" && r.url == "/v1/runtime/media/image")
        .collect();
    assert_eq!(submits.len(), 1, "exactly one image submit: {seen:?}");
    drop(seen);
    let receipts = fx
        .operator("app_run_capability_results", json!({"run_id": run["id"]}))
        .expect("capability receipts");
    let rows = receipts["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "one retained capability receipt: {receipts}");
    assert_eq!(rows[0]["result"]["kind"], "media.generated.image");
    let (page, _next) = fx.shared.store.app_media_catalog(20, None).unwrap();
    assert_eq!(page.len(), 1, "one catalog asset");
    assert_eq!(page[0].projection()["creator"], json!({"kind": "operator"}));
}

/// `app_run_start` is operator-only on the caller gate: an asserted
/// agent or unproven caller refuses BEFORE the team check, any quote
/// compare, binding work or provider submit — no receipt, no asset.
#[test]
fn cad1365_generation_app_run_start_refuses_nonoperator_callers_before_work() {
    let fx = Fx::new();
    let expected = json!({"image": fx.quote()});
    let body = |request_id: &str| {
        json!({"install_id": fx.install, "workflow": "create-image",
            "inputs": {"subject": "Customer follow-up",
                "source": "Acme CRM helps small teams track customers",
                "image_prompt": "Use a calm editorial palette"},
            "request_id": request_id, "expected_quotes": expected})
    };
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        let refused = scoped(who.clone(), || {
            fx.shared
                .dispatch("app_run_start", &body("run-1365-nonop"), std::process::id())
        })
        .expect_err("non-operator app_run_start must refuse");
        assert!(
            refused.to_string().contains("operator"),
            "{who:?} refused by the wrong guard: {refused}"
        );
    }
    let seen = fx.seen.lock().unwrap();
    assert!(
        !seen.iter().any(|r| r.method == "POST"),
        "no provider request may leave a refused caller: {seen:?}"
    );
    drop(seen);
    let (page, _next) = fx.shared.store.app_media_catalog(20, None).unwrap();
    assert!(page.is_empty());
}

/// The agent-workflow team floor stays a floor: `app_run_start` on the
/// valid `team-note` workflow (installed by the same real path) MUST
/// stop at the missing-default-team refusal — before any worker
/// resolution — so a fix for the host-run gap cannot waive teams
/// globally. `expected_quotes` is empty (no capability slots), so a
/// refusal naming the missing team is the honest guard, and no provider
/// traffic exists either way.
#[test]
fn cad1365_generation_app_run_start_keeps_the_team_floor_for_agent_workflows() {
    let fx = Fx::new();
    let refused = fx
        .start(
            "team-note",
            json!({"topic": "release", "writer": "writer"}),
            json!({}),
            "run-1365-teamfloor",
        )
        .expect_err("an agent workflow with no default team must refuse");
    assert!(
        refused.to_string().contains("default team"),
        "the team floor must be the refusing guard: {refused}"
    );
    let seen = fx.seen.lock().unwrap();
    assert!(
        !seen.iter().any(|r| r.method == "POST"),
        "the team floor must refuse before any provider work: {seen:?}"
    );
}

/// CAD-1258: the legacy host-run provider call (the real adapter submit) runs
/// with the PM lock free, and the lock-2 record point is reachable.
#[test]
fn cad1258_host_run_provider_call_runs_outside_the_pm_lock() {
    let fx = Fx::new();
    let expected = json!({"image": fx.quote()});
    let pm_dir = fx._dir.path().join("pm");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let log = seen.clone();
    crate::issue::lockseam::set_hook(Some(Box::new(move |point| {
        let free = crate::issue::Pm::at(&pm_dir)
            .unwrap()
            .try_lock()
            .unwrap()
            .is_some();
        assert!(free, "PM lock held during {point}");
        log.borrow_mut().push(point.to_string());
    })));
    let run = fx.start(
        "create-image",
        json!({"subject": "Customer follow-up",
            "source": "Acme CRM helps small teams track customers",
            "image_prompt": "Use a calm editorial palette"}),
        expected,
        "run-1258-host",
    );
    crate::issue::lockseam::set_hook(None);
    let run = run.expect("host run");
    assert_eq!(run["state"], "succeeded", "{run}");
    assert_eq!(
        *seen.borrow(),
        ["app-host-provider-call", "app-host-before-record"]
    );
}
