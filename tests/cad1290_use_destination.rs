//! CAD-1290: "Use for publishing" creates or replaces the install's
//! publication binding from the company's own destination list. A real
//! in-process daemon over the `test-seam` caller-identity harness and a stub
//! destinations door. These are the implementer's own checks of the ticket
//! outcome; the reviewer writes the acceptance check.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const MINE: &str = "17841400008460056";
const SECOND: &str = "17841400009999999";
const OTHER_FB: &str = "275491372109884";

struct Fx {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

fn rows() -> Value {
    json!({"ok": true, "data": [
        {"connectionId": "c1", "toolkit": "instagram", "displayName": "@harbour",
         "destinationId": MINE, "status": "active", "available": true, "publishable": true},
        {"connectionId": "c5", "toolkit": "instagram", "displayName": "@second",
         "destinationId": SECOND, "status": "active", "available": true, "publishable": true},
        {"connectionId": "c2", "toolkit": "facebook", "displayName": "Harbour page",
         "destinationId": OTHER_FB, "status": "active", "available": true, "publishable": true},
        {"connectionId": "c3", "toolkit": "instagram", "displayName": "@expired",
         "destinationId": "999", "status": "expired", "available": false, "publishable": false},
        {"connectionId": "c4", "toolkit": "instagram", "displayName": "@paused",
         "destinationId": "888", "status": "active", "available": false, "publishable": true},
    ]})
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new().prefix("c1290").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let body = rows().to_string();
        std::thread::spawn(move || {
            for request in stub.incoming_requests() {
                let _ = request.respond(tiny_http::Response::from_string(body.clone()));
            }
        });
        let resolver = MediaResolver::new(
            &format!("http://{addr}"),
            DeviceCredential::new("read-cred".into()),
        )
        .unwrap();
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
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
            social_media_resolver: Some(Arc::new(resolver)),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        let run_dir = dir.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&run_dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&dir, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&dir).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            root,
            stop,
            handle: Some(handle),
        }
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let dir = self.root.path().join("s");
        scoped(who, || client::rpc(&dir, method, params))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|e| panic!("operator {method}: {e}"))
    }
    fn install(&self) -> String {
        let source = self.root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1290\ntitle: C1290\nversion: '0.1.0'\n\
             summary: Publication-slot fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# C1290\n",
        )
        .unwrap();
        std::fs::write(
            source.join("workflows/post.md"),
            "---\ntitle: \"Post\"\ngoal: \"One post\"\npublication_slot: publication\ninputs:\n  writer: { ask: \"writer\" }\n---\n\n## Write\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nWrite one post.\n\n### Acceptance\n- [ ] post exists\n",
        )
        .unwrap();
        self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )["install_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn bindings(&self, install: &str) -> Vec<Value> {
        self.op("app_binding_list", json!({"install_id": install}))["bindings"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
    fn use_params(&self, install: &str, destination: &str, extra: Value) -> Value {
        let mut params = json!({"install_id": install, "destination_id": destination,
            "request_id": "use-1"});
        for (key, value) in extra.as_object().into_iter().flatten() {
            params[key] = value.clone();
        }
        params
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[test]
fn the_destination_list_is_only_active_instagram_accounts_of_the_company() {
    let fx = Fx::start();
    let listed = fx.op("social_destinations", json!({}));
    assert_eq!(listed["unavailable"], false, "{listed}");
    let all = listed["destinations"].as_array().unwrap();
    assert_eq!(all.len(), 2, "{listed}");
    assert_eq!(all[0]["destination_id"], MINE);
    assert_eq!(all[0]["label"], "@harbour");
    assert_eq!(all[1]["destination_id"], SECOND);
}

#[test]
fn use_for_publishing_creates_then_replaces_with_a_revision_check() {
    let fx = Fx::start();
    let install = fx.install();
    let created = fx.op(
        "app_binding_use_destination",
        fx.use_params(&install, MINE, json!({})),
    );
    let publish = &created["binding"]["config"]["publish"];
    assert_eq!(publish["destination_id"], MINE, "{created}");
    assert_eq!(publish["destination_label"], "@harbour");
    assert_eq!(publish["toolkit"], "instagram");
    assert!(
        publish.get("grant_id").is_none(),
        "no standing grant: {created}"
    );
    let revision = created["binding"]["revision"].as_i64().unwrap();
    // Replacing needs the revision the caller saw.
    let no_revision = fx.rpc(
        Asserted::Operator,
        "app_binding_use_destination",
        fx.use_params(&install, MINE, json!({})),
    );
    assert!(no_revision.is_err(), "replaced without a revision");
    let stale = fx.rpc(
        Asserted::Operator,
        "app_binding_use_destination",
        fx.use_params(&install, MINE, json!({"expected_revision": revision + 7})),
    );
    assert!(stale.is_err(), "replaced on a stale revision");
    let replaced = fx.op(
        "app_binding_use_destination",
        fx.use_params(&install, SECOND, json!({"expected_revision": revision})),
    );
    assert_eq!(
        replaced["binding"]["revision"].as_i64().unwrap(),
        revision + 1
    );
    assert_eq!(
        replaced["binding"]["config"]["publish"]["destination_id"],
        SECOND
    );
    assert!(replaced["binding"]["config"]["publish"]
        .get("grant_id")
        .is_none());
    assert_eq!(fx.bindings(&install).len(), 1);
}

#[test]
fn a_destination_outside_the_company_list_is_refused_and_nothing_is_written() {
    let fx = Fx::start();
    let install = fx.install();
    for destination in ["forged-1", OTHER_FB, "999", "888"] {
        let result = fx.rpc(
            Asserted::Operator,
            "app_binding_use_destination",
            fx.use_params(&install, destination, json!({})),
        );
        assert!(result.is_err(), "bound {destination}");
    }
    assert!(fx.bindings(&install).is_empty());
}

#[test]
fn a_request_cannot_bring_its_own_grant_or_label() {
    let fx = Fx::start();
    let install = fx.install();
    for (field, value) in [
        ("grant_id", "dpq_forged_grant_01"),
        ("destination_label", "forged"),
        ("toolkit", "facebook"),
    ] {
        let params = fx.use_params(&install, MINE, json!({ field: value }));
        assert!(
            fx.rpc(Asserted::Operator, "app_binding_use_destination", params)
                .is_err(),
            "accepted {field}"
        );
    }
    assert!(fx.bindings(&install).is_empty());
}

#[test]
fn an_agent_or_unproven_caller_cannot_choose_the_publishing_account() {
    let fx = Fx::start();
    let install = fx.install();
    for who in [Asserted::Agent("cc13-pw".into()), Asserted::Unproven] {
        for (method, params) in [
            (
                "app_binding_use_destination",
                fx.use_params(&install, MINE, json!({})),
            ),
            ("social_destinations", json!({})),
            (
                "social_connect_link",
                json!({"return_to": "https://x.example/"}),
            ),
        ] {
            assert!(
                fx.rpc(who.clone(), method, params).is_err(),
                "{who:?} reached {method}"
            );
        }
    }
    assert!(fx.bindings(&install).is_empty());
}

#[test]
fn a_local_board_gets_no_aos_connect_link() {
    let fx = Fx::start();
    let reply = fx.op(
        "social_connect_link",
        json!({"return_to": "https://x.example/"}),
    );
    assert_eq!(reply, json!({"hosted": false}));
}
