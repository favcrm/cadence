//! CAD-1868 acceptance check, written by the independent Spec/security
//! reviewer from the ticket (operator decisions of 2026-10-27, rounds 1 and
//! 2), NOT by the implementer, who may not edit or weaken it.
//!
//! The rule: "Always allow" on a Designer library-image refine card is a
//! standing grant sealed to the human who decided it. A later agent refine
//! runs with no card only while that human is still the current board owner
//! (the unchanged CAD-1374 fence), acts as that owner (never as the
//! operator), and otherwise parks a card. Only the operator grants or
//! revokes. Decide refuses "Always allow" that the card did not offer. The
//! local-operator marker runs only off a hosted (Native) board.
//!
//! Real guards only. Every step goes through `Shared::dispatch`:
//! `app_assistant_invoke` behind the live-turn verifier (an assigned managed
//! turn whose message durably pins the library image), and
//! `app_assistant_decision` / `app_assistant_permission_revoke` /
//! `app_assistant_permission_block` behind the operator-connection proof.
//! The public owner is a real `operator_auth` public row opened with the
//! supported `open_public_bound` constructor on a Native board; its fresh
//! platform role comes from `membership_lookup_for_test` (the only stub of
//! the central membership answer). The provider is a loopback AgenticOS
//! media door that counts every submit; the library image is produced by
//! the real host run pipeline, and the refine run goes through the real
//! `create_app_run_as` / approve / `dispatch_app_run_as` path.
//!
//! Setup-only store writes, never a guard bypass: case (f) parks cards whose
//! `allow_always` is false, case (h) writes grant rows whose stored human is
//! missing or malformed, and case (g) writes, on a Native board, the exact
//! local marker a Standalone board's real decide stored. Each is then judged
//! by the real RPC exactly as a row the daemon wrote. Every refusal asserts
//! its reason or its parked state, and every "did not run" is paired with a
//! positive control in the same fixture that moves the submit counter.

use super::{ServeOptions, Shared};
use crate::board_identity::Role;
use crate::operator_auth::{BoardUser, PublicRealm};
use crate::platform::{agenticos_external, Custody, Key};
use crate::store::app_records::{AssistantOperationUpdate, AssistantPermissionWrite, RecordStore};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ACCOUNT: &str = "ws_18681868-1868-4868-8868-186818681868";
const GENERATION: &str = "1868aabbccddeeff1868aabbccddeeff";
const COMPANY: &str = "co_cad1868";
const INSTANCE: &str = "cad1868inst";
const HOST: &str = "cad1868.example.test";
const ISSUER: &str = "https://platform.cad1868.example.test";
const OWNER: &str = "synthetic-cad1868-owner";
const REFINE: &str = "media.refine.prepare";
const IMAGE_MODEL: &str = "openai/gpt-image-2.5";
const PAUSED: &str = "Always allow is paused";

const APP_MD: &str = r#"---
app: designer-fixture
title: Designer fixture
version: '1.0.0'
summary: Host image generation and library image refine for CAD-1868.
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

One host image run and one library image refine.
"#;

const CREATE_MD: &str = r#"---
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

The host executes this step itself with the frozen `image` binding.

### Acceptance
- [ ] the provider action used only the frozen `image` binding
"#;

const REFINE_MD: &str = r#"---
title: "Refine image"
goal: "Refine one library image from frozen instructions"
label: Refine image
capability_slots: [image]
execution: host
inputs:
  library_asset_id: { ask: "Library image", example: "asset" }
  library_revision: { ask: "Library revision", example: "1" }
  library_sha256: { ask: "Library original digest", example: "sha256:..." }
  instructions: { ask: "Instructions", example: "Brighten it" }
---

## Refine image
size: S
action: local.capability.call

The host executes this step itself with the frozen `image` binding.

### Acceptance
- [ ] the provider action used only the frozen `image` binding
"#;

const REFINE_ACTION: &str = r#"{ "id": "media.refine.prepare", "description": "Refine a library image.", "input_schema": { "type": "object", "properties": { "asset_id": { "type": "string", "minLength": 1, "maxLength": 64 }, "revision": { "type": "integer", "minimum": 1 }, "sha256": { "type": "string", "minLength": 71, "maxLength": 71 }, "instructions": { "type": "string", "minLength": 1, "maxLength": 2000 } }, "required": ["asset_id", "revision", "sha256", "instructions"], "additionalProperties": false }, "effect": "draft", "confirmation": "permission_required", "availability": "requires_context" }"#;

fn png(seed: u8) -> Vec<u8> {
    use image::ImageEncoder as _;
    let side = 64u32;
    let mut pixels = vec![0u8; (side * side * 3) as usize];
    for (i, px) in pixels.chunks_mut(3).enumerate() {
        px[0] = (i % 251) as u8;
        px[1] = seed;
        px[2] = 0x91;
    }
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(&pixels, side, side, image::ExtendedColorType::Rgb8)
        .unwrap();
    out
}

/// One submitted job: (id, artifact ref, bytes, reference edit).
type Job = (String, String, Vec<u8>, bool);

/// The loopback AgenticOS media door. Every submit is counted and kept.
#[derive(Default)]
struct Door {
    posts: AtomicUsize,
    bodies: Mutex<Vec<Value>>,
    jobs: Mutex<Vec<Job>>,
}

fn price(slug: &str) -> Value {
    json!({"slug":slug,"chargeMinor":31_500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"})
}

fn job_view(id: &str, reference: &str, digest: &str, bytes: usize, edit: bool) -> Value {
    let slug = if edit {
        "generate_image.image-to-image"
    } else {
        "generate_image"
    };
    json!({"id":id,"kind":"image","status":"succeeded",
        "model":IMAGE_MODEL,"provider":"upstream-fixture",
        "providerTaskId":null,
        "artifacts":[{"ref":reference,"digest":digest,"bytes":bytes,"mime":"image/png"}],
        "artifactError":null,"usage":{"providerCredits":null},
        "price":price(slug),"repeated":false,"cached":false,"stale":false})
}

fn serve_door(door: Arc<Door>, stop: Arc<AtomicBool>) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", server.server_addr().to_ip().unwrap());
    let worker = std::thread::spawn(move || loop {
        if stop.load(SeqCst) {
            break;
        }
        let mut request = match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => request,
            Ok(None) => continue,
            Err(_) => break,
        };
        let method = request.method().to_string();
        let url = request.url().to_string();
        let mut body = Vec::new();
        let _ = std::io::Read::read_to_end(request.as_reader(), &mut body);
        let json_reply = |status: u16, value: Value| {
            tiny_http::Response::from_data(value.to_string().into_bytes())
                .with_status_code(status)
                .with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                )
        };
        let reply = match (method.as_str(), url.as_str()) {
            ("GET", "/v1/runtime/media/price/image") => json_reply(
                200,
                json!({"ok":true,"data":{"kind":"image","model":IMAGE_MODEL,"price":price("generate_image")}}),
            ),
            ("GET", "/v1/runtime/media/price/image?variant=image-to-image") => json_reply(
                200,
                json!({"ok":true,"data":{"kind":"image","model":IMAGE_MODEL,"price":price("generate_image.image-to-image")}}),
            ),
            ("POST", "/v1/runtime/media/image") => {
                let sent: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let n = door.posts.fetch_add(1, SeqCst) + 1;
                door.bodies.lock().unwrap().push(sent.clone());
                let edit = sent.get("references").is_some();
                let bytes = png(n as u8);
                let digest = format!("{:x}", Sha256::digest(&bytes));
                let id = format!("med_cad1868job{n}");
                let reference = format!("{id}.{digest}");
                let view = job_view(&id, &reference, &digest, bytes.len(), edit);
                door.jobs.lock().unwrap().push((id, reference, bytes, edit));
                json_reply(201, json!({"ok":true,"data":{"job":view}}))
            }
            ("GET", u) if u.starts_with("/v1/runtime/media/jobs/") => {
                let id = &u["/v1/runtime/media/jobs/".len()..];
                let jobs = door.jobs.lock().unwrap();
                match jobs.iter().find(|(j, ..)| j == id) {
                    Some((id, reference, bytes, edit)) => {
                        let digest = format!("{:x}", Sha256::digest(bytes));
                        json_reply(
                            200,
                            json!({"ok":true,"data":{"job":job_view(id, reference, &digest, bytes.len(), *edit)}}),
                        )
                    }
                    None => json_reply(404, json!({"ok":false,"error":{"code":"not_found"}})),
                }
            }
            ("GET", u) if u.starts_with("/v1/runtime/media/artifacts/") => {
                let reference = &u["/v1/runtime/media/artifacts/".len()..];
                let jobs = door.jobs.lock().unwrap();
                match jobs.iter().find(|(_, r, ..)| r == reference) {
                    Some((_, _, bytes, _)) => tiny_http::Response::from_data(bytes.clone())
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                        ),
                    None => json_reply(404, json!({"ok":false,"error":{"code":"not_found"}})),
                }
            }
            _ => json_reply(404, json!({"ok":false,"error":{"code":"not_found"}})),
        };
        let _ = request.respond(reply);
    });
    (base, worker)
}

struct Fx {
    dir: tempfile::TempDir,
    shared: Arc<Shared>,
    door: Arc<Door>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    clock: Arc<AtomicI64>,
    /// The fresh platform role the membership answer gives the owner.
    role: Arc<Mutex<Option<Role>>>,
    install: String,
    context: String,
    asset_id: String,
    revision: i64,
    sha256: String,
    message: String,
    token: String,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn refused(result: crate::Result<Value>, needle: &str, what: &str) -> String {
    match result {
        Ok(value) => panic!("{what} was accepted: {value}"),
        Err(error) => {
            let text = error.to_string();
            assert!(
                text.contains(needle),
                "{what} refused for the wrong reason (want '{needle}'): {text}"
            );
            text
        }
    }
}

/// The board's membership placement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Standalone,
    Native,
    /// Hosted placement signalled but its context is partial.
    RequiredUnavailable,
}

impl Fx {
    /// `native`: a platform-placed (hosted) board; otherwise Standalone.
    fn boot(native: bool) -> Self {
        Self::boot_mode(if native {
            Mode::Native
        } else {
            Mode::Standalone
        })
    }

    fn boot_mode(mode: Mode) -> Self {
        let dir = tempfile::Builder::new().prefix("c1868").tempdir().unwrap();
        let (state, pm) = (dir.path().to_path_buf(), dir.path().join("pm"));
        crate::issue::Pm::init(&pm).unwrap();
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: HOST.into(),
                issuer: ISSUER.into(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let door = Arc::new(Door::default());
        let (base, worker) = serve_door(door.clone(), stop.clone());
        let clock = Arc::new(AtomicI64::new(crate::issue::time::now_epoch()));
        let tick = clock.clone();
        let mut opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            operator_clock: Some(Arc::new(move || tick.load(SeqCst))),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.provider_env.set("CADENCE_TEST_MEDIA_POLL_MS", "20");
        opts.provider_env
            .set("CADENCE_TEST_MEDIA_DEADLINE_MS", "8000");
        if mode == Mode::RequiredUnavailable {
            // A company marker without its instance: placement is signalled
            // but incomplete.
            opts.provider_env.set("AGENTICOS_COMPANY", COMPANY);
        }
        if mode == Mode::Native {
            for (name, value) in [
                ("AGENTICOS_COMPANY", COMPANY),
                ("AGENTICOS_INSTANCE", INSTANCE),
                ("AGENTICOS_BOARD_HOST", HOST),
                ("AGENTICOS_BOARD_ISSUER", ISSUER),
                ("AGENTICOS_BOARD_COMPANY", COMPANY),
            ] {
                opts.provider_env.set(name, value);
            }
        }
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
        let mut shared = Shared::new(&state, &opts).unwrap();
        let actual = match shared.membership_mode {
            super::public_membership::MembershipMode::Standalone => Mode::Standalone,
            super::public_membership::MembershipMode::Native(_) => Mode::Native,
            super::public_membership::MembershipMode::RequiredUnavailable => {
                Mode::RequiredUnavailable
            }
        };
        assert_eq!(actual, mode, "fixture BLOCKED: wrong membership mode");
        let role = Arc::new(Mutex::new(Some(Role::Operator)));
        {
            let fresh = Arc::get_mut(&mut shared).expect("fresh unique Shared");
            fresh.platform_custody = Custody::file(dir.path().join("fixture-custody"));
            let answer = role.clone();
            fresh.membership_lookup_for_test =
                Some(Box::new(move |_hash, sub, company, instance| {
                    assert!(
                        company == COMPANY && instance == INSTANCE,
                        "fixture BLOCKED: wrong membership binding"
                    );
                    Ok(if sub == OWNER {
                        *answer.lock().unwrap()
                    } else {
                        None
                    })
                }));
        }
        let secret = b"synthetic-cad1868-token";
        let key = Key {
            platform: agenticos_external::PLATFORM,
            account: ACCOUNT,
        };
        let custody = shared.platform_custody.put(&key, secret).unwrap();
        shared
            .store
            .platform_enroll(
                &crate::store::CredentialRecord {
                    connection_id: "gen1868".into(),
                    credential_revision: 1,
                    platform: agenticos_external::PLATFORM.into(),
                    account: ACCOUNT.into(),
                    scopes: vec!["provider.draft".into()],
                    fingerprint: crate::secret::fingerprint(secret),
                    custody: custody.into(),
                    exchange: "token".into(),
                    enrolled_at: 1.0,
                    by: "operator".into(),
                },
                false,
                None,
            )
            .unwrap();
        let cwd = state.to_str().unwrap();
        for alias in ["master", "w1"] {
            shared
                .store
                .register_agent(&crate::store::NewAgent {
                    alias,
                    provider: "pi",
                    endpoint_kind: "managed",
                    role: "worker",
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: (alias == "master").then_some(r#"{"model":"fixture/model"}"#),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
        shared
            .store
            .set_identity_with_quota(
                "master",
                &crate::adapter::Identity {
                    thread_id: "cad1868-thread".into(),
                    session_id: "cad1868-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some(GENERATION.into()),
                    attach: None,
                },
                None,
            )
            .unwrap();
        let mut fx = Self {
            dir,
            shared,
            door,
            stop,
            worker: Some(worker),
            clock,
            role,
            install: String::new(),
            context: String::new(),
            asset_id: String::new(),
            revision: 0,
            sha256: String::new(),
            message: String::new(),
            token: String::new(),
        };
        fx.install_and_produce();
        fx.message = "cad1868-turn".into();
        fx.token = fx.turn(&fx.message.clone());
        fx
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }

    fn operator(&self, method: &str, params: Value) -> Value {
        self.call(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }

    fn records(&self) -> RecordStore {
        RecordStore::open(self.dir.path(), &self.install).unwrap()
    }

    fn install_and_produce(&mut self) {
        let app = self.dir.path().join("app");
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(app.join("app.md"), APP_MD).unwrap();
        std::fs::write(app.join("workflows/create-image.md"), CREATE_MD).unwrap();
        std::fs::write(app.join("workflows/refine-image.md"), REFINE_MD).unwrap();
        std::fs::write(
            app.join("app-assistant.json"),
            format!(r#"{{ "contract": "app-assistant/v1", "actions": [{REFINE_ACTION}] }}"#),
        )
        .unwrap();
        let installed = self.operator(
            "app_workspace_install",
            json!({"source": app.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        self.operator(
            "app_local_install_approve",
            json!({"install_id": install, "digest": installed["digest"]}),
        );
        let ctx = self.operator(
            "app_context_create",
            json!({"install_id": install, "label": "media",
                "input_defaults": {}, "request_id": "ctx-1868"}),
        );
        let context = ctx["context"]["id"]
            .as_str()
            .or_else(|| ctx["id"].as_str())
            .unwrap()
            .to_string();
        self.operator(
            "app_binding_create",
            json!({"install_id": install, "context_id": context, "slot": "image",
                "connection_id": "gen1868", "request_id": "bind-1868"}),
        );
        let created = self.operator(
            "app_run_create",
            json!({"install_id": install, "context_id": context, "workflow": "create-image",
                "inputs": {"subject": "Harbour mark",
                    "source": "A red lighthouse marks the harbour",
                    "image_prompt": "Use an editorial palette"},
                "request_id": "run-1868"}),
        );
        let run = created["id"].as_str().unwrap().to_string();
        self.operator(
            "app_run_approve",
            json!({"run_id": run, "digest": created["snapshot_digest"]}),
        );
        let done = self.operator("app_run_dispatch", json!({"run_id": run}));
        assert_eq!(done["state"], "succeeded", "library image run: {done}");
        let (page, _) = self.shared.store.app_media_catalog(20, None).unwrap();
        assert_eq!(page.len(), 1, "one retained library image");
        let image = self
            .shared
            .store
            .app_media_context_image(&page[0].id)
            .unwrap();
        self.asset_id = page[0].id.clone();
        self.revision = page[0].revision;
        self.sha256 = image.original.sha256.clone();
        self.install = install;
        self.context = context;
        assert_eq!(self.door.posts.load(SeqCst), 1, "setup submitted once");
    }

    /// An assigned managed turn whose message durably pins the library image.
    fn turn(&self, message: &str) -> String {
        self.operator(
            "conversation_create",
            json!({"alias":"master", "install_id":self.install, "context_id":self.context, "general":true}),
        );
        self.operator(
            "thread_send",
            json!({"alias":"master", "text":"Brighten this image", "message":message,
                "app":{"install_id":self.install,"context_id":self.context},
                "media_refs":[{"asset_id":self.asset_id,"revision":self.revision,"sha256":self.sha256}]}),
        );
        let db = rusqlite::Connection::open(crate::rollout::db_file(self.dir.path())).unwrap();
        db.execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
            [message],
        )
        .unwrap();
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS.mint(GENERATION);
        self.shared.store.mark_running(message, &token).unwrap();
        token
    }

    fn input(&self) -> Value {
        json!({"asset_id":self.asset_id,"revision":self.revision,
            "sha256":self.sha256,"instructions":"Brighten it"})
    }

    /// The agent's refine call on its live turn.
    fn invoke(&self, op: &str) -> crate::Result<Value> {
        self.call(
            Asserted::Agent("master".into()),
            "app_assistant_invoke",
            json!({"install_id":self.install,"context_id":self.context,"message":self.message,
                "token":self.token,"action_id":REFINE,"operation_id":op,"input":self.input()}),
        )
    }

    fn decide(
        &self,
        who: Asserted,
        op: &Value,
        decision: &str,
        session: Option<&str>,
    ) -> crate::Result<Value> {
        let mut params = json!({"install_id":self.install,"context_id":self.context,
            "operation_id":op["id"],"decision":decision,"expected_revision":op["revision"]});
        if let Some(session) = session {
            params["public_session"] = json!(session);
        }
        self.call(who, "app_assistant_decision", params)
    }

    fn show(&self, op: &str) -> Value {
        self.operator(
            "app_assistant_operation_operator_show",
            json!({"install_id":self.install,"context_id":self.context,"operation_id":op}),
        )["operation"]
            .clone()
    }

    fn grants(&self) -> Vec<Value> {
        self.operator(
            "app_assistant_permissions",
            json!({"install_id":self.install,"context_id":self.context}),
        )["permissions"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn refine_grant(&self) -> Option<Value> {
        self.grants()
            .into_iter()
            .find(|g| g["action_id"] == REFINE && g["effect"] == "allow" && g["state"] == "active")
    }

    fn posts(&self) -> usize {
        self.door.posts.load(SeqCst)
    }

    /// A real public owner row on this board; returns (token, sealed human).
    fn owner(&self, jti: &str) -> (String, Value) {
        let user = BoardUser {
            sub: OWNER.into(),
            email: "cad1868-owner@example.test".into(),
            name: "Synthetic Designer owner".into(),
            role: "operator".into(),
            handle: OWNER.into(),
        };
        let display = user.actor();
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                user,
                jti,
                now + 30 * 24 * 3600,
                "cad1868-acceptance",
                PublicRealm {
                    issuer: ISSUER.into(),
                    company: COMPANY.into(),
                    host: HOST.into(),
                },
                now,
            )
            .unwrap()
            .expect("fresh jti");
        let human = json!({"sub": OWNER, "company": COMPANY,
            "session": opened.session.id, "role": "operator", "display": display});
        (opened.token, human)
    }

    /// The grant digest the daemon pins a refine grant to, re-derived from
    /// the reviewed inputs (contract, action, schema, handler, descriptor).
    fn grant_digest(&self) -> String {
        let descriptor = self.operator(
            "app_assistant_actions_operator",
            json!({"install_id":self.install,"context_id":self.context}),
        )["descriptor_digest"]
            .as_str()
            .unwrap()
            .to_string();
        let meta = crate::app_assistant::registered_action(REFINE).unwrap();
        crate::store::app_runs::material_digest(
            &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":REFINE,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor}),
        )
    }

    /// Setup only: an active refine grant row with this stored human.
    fn write_grant(&self, sealed: Option<&str>) {
        self.records()
            .app_assistant_permission_save(AssistantPermissionWrite {
                permission_id: &uuid::Uuid::new_v4().to_string(),
                context: &self.context,
                action_id: REFINE,
                resource_id: "any",
                effect: "allow",
                semantics_digest: &self.grant_digest(),
                scope_label: "Refining library images",
                sealed_human: sealed,
            })
            .unwrap();
    }

    /// The owner decides "Always allow" on a fresh card; the decided run
    /// itself succeeds. Returns the owner's token and sealed human.
    fn owner_grants(&self, op: &str) -> (String, Value) {
        let (token, human) = self.owner(&format!("jti-{op}"));
        let card = pending(&self.invoke(op).unwrap());
        let before = self.posts();
        let done = self
            .decide(Asserted::Operator, &card, "allow_always", Some(&token))
            .unwrap();
        assert_eq!(done["operation"]["status"], "succeeded", "{done}");
        assert_eq!(self.posts(), before + 1, "the decided refine submits once");
        assert!(self.refine_grant().is_some(), "owner grant saved");
        (token, human)
    }

    fn run(&self, reply: &Value) -> Value {
        let id = reply["operation"]["result"]["run_id"]
            .as_str()
            .unwrap_or_else(|| panic!("no run id: {reply}"));
        self.shared.store.app_run_show(id).unwrap()
    }
}

/// A parked refine card that offers "Always allow".
fn pending(reply: &Value) -> Value {
    let op = reply["operation"].clone();
    assert_eq!(op["status"], "pending_permission", "{reply}");
    assert_eq!(op["action_id"], REFINE, "{reply}");
    assert_eq!(op["permission_request"]["allow_always"], true, "{reply}");
    op
}

/// A reply that ran under the grant with no card.
fn ran_without_card(reply: &Value, what: &str) {
    let op = &reply["operation"];
    assert_eq!(op["status"], "succeeded", "{what}: {reply}");
    assert!(
        op["permission_request"].is_null(),
        "{what} still carries a card: {reply}"
    );
}

/// The agent's refine parked a "paused" card and submitted nothing.
fn parked_paused(fx: &Fx, op: &str, posts: usize, what: &str) {
    let reply = fx
        .invoke(op)
        .unwrap_or_else(|e| panic!("{what}: the agent call errored instead of parking: {e}"));
    let card = pending(&reply);
    assert!(
        card["summary"].as_str().unwrap_or("").contains(PAUSED),
        "{what}: the card does not say the grant is paused: {reply}"
    );
    assert!(card["result"].is_null(), "{what}: {reply}");
    assert_eq!(fx.posts(), posts, "{what}: a refine was submitted");
}

/// (a) The public owner's "Always allow" lets a later agent refine run with
/// no card, sealed to that owner: the run's creator is the owner (never the
/// operator) and its approval carries the stored human. A replay never
/// submits again.
#[test]
fn a_owner_allow_always_runs_the_next_agent_refine_as_that_owner() {
    let fx = Fx::boot(true);
    let (_token, human) = fx.owner_grants("op-a1");
    let grant = fx.refine_grant().unwrap();
    assert_eq!(grant["resource_id"], "any", "{grant}");
    assert!(
        grant.get("sealed_human").is_none(),
        "the sealed human leaked into the listing: {grant}"
    );

    let before = fx.posts();
    let ran = fx.invoke("op-a2").unwrap();
    ran_without_card(&ran, "granted agent refine");
    assert_eq!(fx.posts(), before + 1, "the granted refine submits once");
    let run = fx.run(&ran);
    assert_eq!(
        run["snapshot"]["creator"],
        json!({"kind":"user","id":OWNER}),
        "the granted run must act as the owner: {run}"
    );
    assert_eq!(
        run["approval"]["human"], human,
        "the granted run's sealed approval must be the stored owner: {run}"
    );
    assert_ne!(run["approval"]["human"], json!({"kind":"operator"}));
    let body = fx.door.bodies.lock().unwrap().last().cloned().unwrap();
    assert!(body["references"].is_array(), "a reference edit: {body}");

    // Replay of the granted operation: same outcome, no new submit.
    let replay = fx.invoke("op-a2").unwrap();
    assert_eq!(replay["operation"]["status"], "succeeded", "{replay}");
    assert_eq!(fx.posts(), before + 1, "a replay submitted again");
}

/// (b) Once the owner's board session is no longer current — idle expiry on
/// the test clock, or sign-out — the next agent refine parks a card and
/// submits nothing.
#[test]
fn b_after_the_owner_session_ends_the_next_agent_refine_parks() {
    let fx = Fx::boot(true);
    let (token, _) = fx.owner_grants("op-b1");
    let ran = fx.invoke("op-b2").unwrap();
    ran_without_card(&ran, "control: granted refine while signed in");
    let posts = fx.posts();

    // Idle expiry: 24 hours and a second with no use of the session.
    fx.clock
        .fetch_add(crate::operator_auth::IDLE_SECS + 1, SeqCst);
    parked_paused(&fx, "op-b3", posts, "after idle expiry");

    // Sign-out of a fresh owner session that re-granted.
    let fx = Fx::boot(true);
    let (token2, _) = fx.owner_grants("op-b4");
    let _ = token;
    let ran = fx.invoke("op-b5").unwrap();
    ran_without_card(&ran, "control: granted refine before sign-out");
    let posts = fx.posts();
    assert!(fx.shared.operator_auth().revoke_token(&token2).unwrap());
    parked_paused(&fx, "op-b6", posts, "after sign-out");
}

/// (c) A grant whose stored human is no longer the owner — demoted to a
/// member, or no longer a member at all — refuses and parks; it never runs
/// as the operator.
#[test]
fn c_a_grant_whose_human_lost_ownership_parks_and_never_runs_as_operator() {
    let fx = Fx::boot(true);
    fx.owner_grants("op-c1");
    let ran = fx.invoke("op-c2").unwrap();
    ran_without_card(&ran, "control: granted refine while owner");
    let posts = fx.posts();

    *fx.role.lock().unwrap() = Some(Role::Member);
    parked_paused(&fx, "op-c3", posts, "owner demoted to member");

    *fx.role.lock().unwrap() = None;
    parked_paused(&fx, "op-c4", posts, "owner no longer a member");

    // A stored human naming another subject of the same company, whose
    // session reference is the real owner's live row, is not that owner.
    *fx.role.lock().unwrap() = Some(Role::Operator);
    let (_, mut other) = fx.owner("jti-c5");
    other["sub"] = json!("someone-else");
    fx.write_grant(Some(&other.to_string()));
    parked_paused(&fx, "op-c5", posts, "a different sub");
    let (_, mut foreign) = fx.owner("jti-c6");
    foreign["company"] = json!("co_other_company");
    fx.write_grant(Some(&foreign.to_string()));
    parked_paused(&fx, "op-c6", posts, "a different company");
    for op in ["op-c3", "op-c4", "op-c5", "op-c6"] {
        assert_eq!(fx.show(op)["status"], "pending_permission", "{op}");
    }
}

/// (d) An agent caller cannot decide "Always allow" on a refine card, cannot
/// create a grant by any operator verb, and cannot revoke one — even when it
/// carries the owner's board session.
#[test]
fn d_an_agent_cannot_decide_always_create_or_revoke_a_refine_grant() {
    let fx = Fx::boot(true);
    let (token, _) = fx.owner("jti-d0");
    let card = pending(&fx.invoke("op-d1").unwrap());
    refused(
        fx.decide(
            Asserted::Agent("master".into()),
            &card,
            "allow_always",
            None,
        ),
        "not app_assistant_decision",
        "the requesting agent's Always allow",
    );
    refused(
        fx.decide(
            Asserted::Agent("master".into()),
            &card,
            "allow_always",
            Some(&token),
        ),
        "not app_assistant_decision",
        "the requesting agent's Always allow carrying the owner's session",
    );
    refused(
        fx.decide(
            Asserted::Agent("w1".into()),
            &card,
            "allow_always",
            Some(&token),
        ),
        "operator action",
        "another agent's Always allow carrying the owner's session",
    );
    refused(
        fx.decide(Asserted::Unproven, &card, "allow_always", Some(&token)),
        "operator",
        "an unproven Always allow",
    );
    refused(
        fx.call(
            Asserted::Agent("w1".into()),
            "app_assistant_permission_block",
            json!({"install_id":fx.install,"context_id":fx.context,"action_id":REFINE,"resource_id":"any"}),
        ),
        "operator action",
        "an agent standing-rule write",
    );
    assert!(fx.grants().is_empty(), "an agent created a grant");
    assert_eq!(fx.posts(), 1, "a refused decision submitted");
    assert_eq!(fx.show("op-d1")["status"], "pending_permission");

    // Positive control: the owner grants; no agent can revoke it.
    fx.decide(Asserted::Operator, &card, "allow_always", Some(&token))
        .unwrap();
    let grant = fx.refine_grant().expect("owner grant");
    let revoke = json!({"install_id":fx.install,"context_id":fx.context,
        "permission_id":grant["id"],"expected_revision":grant["revision"]});
    refused(
        fx.call(
            Asserted::Agent("master".into()),
            "app_assistant_permission_revoke",
            revoke.clone(),
        ),
        "not app_assistant_permission_revoke",
        "the agent's revoke",
    );
    refused(
        fx.call(
            Asserted::Agent("w1".into()),
            "app_assistant_permission_revoke",
            revoke,
        ),
        "operator action",
        "another agent's revoke",
    );
    assert!(fx.refine_grant().is_some(), "an agent revoked the grant");
}

/// (e) After the operator revokes the refine grant, the next agent refine
/// parks a card and submits nothing.
#[test]
fn e_after_an_operator_revoke_the_next_agent_refine_parks() {
    let fx = Fx::boot(true);
    fx.owner_grants("op-e1");
    let ran = fx.invoke("op-e2").unwrap();
    ran_without_card(&ran, "control: granted refine before revoke");
    let posts = fx.posts();
    let grant = fx.refine_grant().unwrap();
    let revoked = fx.operator(
        "app_assistant_permission_revoke",
        json!({"install_id":fx.install,"context_id":fx.context,
            "permission_id":grant["id"],"expected_revision":grant["revision"]}),
    );
    assert_eq!(revoked["permission"]["state"], "revoked", "{revoked}");
    let card = pending(&fx.invoke("op-e3").unwrap());
    assert!(card["result"].is_null());
    assert_eq!(fx.posts(), posts, "a refine after revoke was submitted");
}

/// (f) Decide refuses "Always allow" when the parked card did not offer it,
/// with the explicit reason, for a refine card and for any other action.
#[test]
fn f_decide_refuses_allow_always_the_card_did_not_offer() {
    let fx = Fx::boot(true);
    let (token, _) = fx.owner("jti-f0");
    let records = fx.records();
    let descriptor = fx.operator(
        "app_assistant_actions_operator",
        json!({"install_id":fx.install,"context_id":fx.context}),
    )["descriptor_digest"]
        .as_str()
        .unwrap()
        .to_string();
    for (op, action, input) in [
        ("op-f1", REFINE, fx.input()),
        (
            "op-f2",
            "customer.tags.update",
            json!({"customer_id":"c1","expected_revision":1,"tags":["vip"]}),
        ),
    ] {
        let meta = crate::app_assistant::registered_action(action).unwrap();
        let digest = crate::store::app_runs::material_digest(
            &json!({"contract":crate::issue::app_assistant::CONTRACT,"action":action,"schema":meta.input_schema,"handler_version":meta.handler_version,"descriptor_digest":descriptor,"input":input}),
        );
        records
            .app_assistant_operation_create(op, &fx.context, action, &digest, &input)
            .unwrap();
        let parked = records
            .app_assistant_operation_set(AssistantOperationUpdate {
                operation_id: op,
                context: &fx.context,
                expected_revision: 1,
                status: "pending_permission",
                summary: "Review this request",
                result: &Value::Null,
                resource_refs: &json!([]),
                permission_request: &json!({"reason":"review","scope":{"install_id":fx.install,
                    "context_id":fx.context,"action_id":action,"resource_id":"r1"},
                    "allow_always":false,"preview":{}}),
                error: None,
            })
            .unwrap();
        for session in [Some(token.as_str()), None] {
            refused(
                fx.decide(Asserted::Operator, &parked, "allow_always", session),
                "did not offer Always allow",
                &format!("Always allow on a {action} card that did not offer it"),
            );
        }
        assert_eq!(fx.show(op)["status"], "pending_permission");
    }
    assert!(fx.grants().is_empty(), "a grant was created");
    assert_eq!(fx.posts(), 1, "a refused decision submitted");
    // Control: a card that offers it is accepted by the same verb.
    let card = pending(&fx.invoke("op-f3").unwrap());
    let done = fx
        .decide(Asserted::Operator, &card, "allow_always", Some(&token))
        .unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
}

/// (g) A grant decided from a local operator connection stores the explicit
/// local marker and runs locally; the same stored marker on a hosted
/// (Native) or a `RequiredUnavailable` board refuses and parks, and a local
/// decide cannot write it there. Only Standalone honours it.
#[test]
fn g_the_local_marker_runs_locally_and_parks_on_a_hosted_board() {
    let local = Fx::boot(false);
    let card = pending(&local.invoke("op-g1").unwrap());
    assert_eq!(
        card["permission_request"]["allow_always_scope"], "installation",
        "a Standalone card states an installation-wide grant: {card}"
    );
    let done = local
        .decide(Asserted::Operator, &card, "allow_always", None)
        .unwrap();
    assert_eq!(done["operation"]["status"], "succeeded", "{done}");
    let grant = local.refine_grant().expect("local grant");
    let marker = local
        .records()
        .app_assistant_permission_sealed_human(&local.context, grant["id"].as_str().unwrap())
        .unwrap()
        .expect("the local grant stores its marker");
    let before = local.posts();
    let ran = local.invoke("op-g2").unwrap();
    ran_without_card(&ran, "local operator's granted refine");
    assert_eq!(local.posts(), before + 1);

    let hosted = Fx::boot(true);
    let card = pending(&hosted.invoke("op-g3").unwrap());
    assert_eq!(card["permission_request"]["allow_always_scope"], "session");
    refused(
        hosted.decide(Asserted::Operator, &card, "allow_always", None),
        "needs the signed-in board owner",
        "a local Always allow on a hosted board",
    );
    assert!(
        hosted.grants().is_empty(),
        "a local grant was saved on a hosted board"
    );
    hosted.write_grant(Some(&marker));
    parked_paused(
        &hosted,
        "op-g4",
        hosted.posts(),
        "the local marker on a hosted board",
    );

    // Hosted placement signalled but incomplete (`RequiredUnavailable`) is
    // not Standalone: the marker is refused when saved and parks when run.
    let broken = Fx::boot_mode(Mode::RequiredUnavailable);
    let card = pending(&broken.invoke("op-g5").unwrap());
    assert_eq!(card["permission_request"]["allow_always_scope"], "session");
    refused(
        broken.decide(Asserted::Operator, &card, "allow_always", None),
        "needs the signed-in board owner",
        "a local Always allow on a RequiredUnavailable board",
    );
    assert!(
        broken.grants().is_empty(),
        "a local grant was saved on a RequiredUnavailable board"
    );
    assert_eq!(broken.show("op-g5")["status"], "pending_permission");
    broken.write_grant(Some(&marker));
    parked_paused(
        &broken,
        "op-g6",
        broken.posts(),
        "the local marker on a RequiredUnavailable board",
    );
    // Control: the operator's allow-once still runs there, so the parks
    // above are the marker rule, not a broken fixture.
    let once = broken
        .decide(Asserted::Operator, &card, "allow_once", None)
        .unwrap();
    assert_eq!(once["operation"]["status"], "succeeded", "{once}");
}

/// (h) An active refine grant whose stored human is missing or malformed
/// refuses and parks; it is never read as the local operator.
#[test]
fn h_a_missing_or_malformed_sealed_human_parks() {
    let fx = Fx::boot(true);
    let (_, human) = fx.owner("jti-h0");
    let mut empty_sub = human.clone();
    empty_sub["sub"] = json!("");
    let mut four = human.clone();
    four.as_object_mut().unwrap().remove("display");
    let mut six = human.clone();
    six["kind"] = json!("local_operator");
    let posts = fx.posts();
    let cases: [(&str, Option<String>); 7] = [
        ("NULL", None),
        ("not JSON", Some("{not json".into())),
        ("JSON null", Some("null".into())),
        ("empty sub", Some(empty_sub.to_string())),
        ("missing display", Some(four.to_string())),
        ("an extra field", Some(six.to_string())),
        ("an operator actor", Some(r#"{"kind":"operator"}"#.into())),
    ];
    for (i, (what, sealed)) in cases.iter().enumerate() {
        fx.write_grant(sealed.as_deref());
        parked_paused(&fx, &format!("op-h{i}"), posts, what);
    }
    // Control: the same row carrying the real owner's human runs.
    fx.write_grant(Some(&human.to_string()));
    let ran = fx.invoke("op-h-ok").unwrap();
    ran_without_card(&ran, "control: a well-formed owner grant");
    assert_eq!(fx.posts(), posts + 1);
}
