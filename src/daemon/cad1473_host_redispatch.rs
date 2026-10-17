//! CAD-1473: real account claim/dispatch with a test-only provider-entry barrier.
//! Synthetic saved login and TLS only; no installed state or paid generation.
use super::*;
use crate::platform::{account_media, PlatformAdapter};
use crate::remote_cli::cad1459_account_image_acceptance as login;
use crate::test_seam::{scoped, Asserted};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Condvar};

const TEST: &str =
    "daemon::cad1473_host_redispatch::host_account_run_monitor_leaves_live_provider_alone";
const CHILD: &str = "CAD1473_TEST_ROOT";

/// Only the provider entry is gated; quotes, custody, claims, authority,
/// network response parsing and receipt retention are the real implementations.
struct BlockedProvider {
    inner: account_media::AccountMediaAdapter,
    entered: mpsc::SyncSender<()>,
    released: Arc<(Mutex<bool>, Condvar)>,
}

impl PlatformAdapter for BlockedProvider {
    fn table(&self) -> &crate::contract_fixture::ToolTable {
        self.inner.table()
    }
    fn connection_descriptor(&self) -> Option<crate::platform::connections::ProviderDescriptor> {
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn app_credentialless_account(&self, account: &str) -> bool {
        self.inner.app_credentialless_account(account)
    }
    fn quote_app_capability(
        &self,
        credential: &[u8],
        binding: &Value,
    ) -> std::result::Result<crate::platform::AppCapabilityQuote, String> {
        self.inner.quote_app_capability(credential, binding)
    }
    fn execute_app_capability_outcome(
        &self,
        credential: &[u8],
        authority: &Value,
        input: &Value,
        key: &str,
    ) -> std::result::Result<
        crate::platform::AppCapabilityOutput,
        crate::platform::AppCapabilityError,
    > {
        self.entered.send(()).unwrap();
        let (lock, ready) = &*self.released;
        let released = lock.lock().unwrap();
        let (released, timeout) = ready
            .wait_timeout_while(released, Duration::from_secs(20), |released| !*released)
            .unwrap();
        assert!(!timeout.timed_out(), "provider was never released");
        drop(released);
        self.inner
            .execute_app_capability_outcome(credential, authority, input, key)
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
        expected_hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.inner
            .execute(credential, tool, input, key, expected_hash)
    }
    fn read_back(&self, tool: &str, input: &Value) -> crate::contract_fixture::Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
}

fn op(shared: &Arc<Shared>, method: &str, params: Value) -> Value {
    scoped(Asserted::Operator, || {
        shared.dispatch(method, &params, std::process::id())
    })
    .unwrap_or_else(|error| panic!("{method}: {error}"))
}

fn install(shared: &Arc<Shared>, root: &Path) -> String {
    let app = root.join("app");
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(app.join("app.md"), "---\napp: redispatch-fixture\ntitle: Redispatch fixture\nversion: '1.0.0'\nneeds:\n  connections: []\n  capabilities:\n    image:\n      schema: 1\n      capability: media.generate\n      version: 1\n      action: generate_image\n      resource_kind: connection_account\n      effect: draft\n---\nOne host image.\n").unwrap();
    std::fs::write(app.join("workflows/image.md"), "---\ntitle: Image\ngoal: Generate a reviewed image\ncapability_slots: [image]\nexecution: host\ninputs:\n  subject: { ask: 'Subject', example: 'A calm landscape' }\n  source: { ask: 'Source facts', example: 'The hills are green' }\n  image_prompt: { ask: 'Image guidance', example: 'An editorial palette' }\n---\n\n## Generate image\nsize: S\naction: local.capability.call\n\nGenerate one image from the frozen binding.\n\n### Acceptance\n- [ ] retain the provider receipt\n").unwrap();
    let installed = op(shared, "app_workspace_install", json!({"source": app}));
    let id = installed["install_id"].as_str().unwrap().to_string();
    op(
        shared,
        "app_local_install_approve",
        json!({"install_id": id, "digest": installed["digest"]}),
    );
    id
}

fn receipts(shared: &Shared, id: &str) -> Value {
    let db = rusqlite::Connection::open(shared.state_dir.join("cadence.sqlite3")).unwrap();
    let claims: usize = db
        .query_row(
            "SELECT count(*) FROM app_capability_claims WHERE run_id=?",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let failures: usize = db
        .query_row(
            "SELECT count(*) FROM app_run_failures WHERE run_id=?",
            [id],
            |row| row.get(0),
        )
        .unwrap();
    let mut receipts = shared.store.app_capability_results(id).unwrap();
    receipts["claims"] = json!(claims);
    receipts["failures"] = json!(failures);
    receipts
}

fn approved_run(shared: &Arc<Shared>, install: &str, request: &str) -> Value {
    let run = op(
        shared,
        "app_run_create",
        json!({"install_id": install,
        "workflow": "image", "inputs": {"subject": "A calm landscape", "source": "The hills are green", "image_prompt": "An editorial palette"}, "request_id": request}),
    );
    op(
        shared,
        "app_run_approve",
        json!({"run_id": run["id"], "digest": run["snapshot_digest"]}),
    )
}

#[test]
fn host_account_run_monitor_leaves_live_provider_alone() {
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = tempfile::Builder::new().prefix("c1473-").tempdir().unwrap();
        for dir in ["home", "config", "data", "state", "tmp", "locks"] {
            login::private_dir(&root.path().join(dir));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
            .env(CHILD, root.path())
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("TMPDIR", root.path().join("tmp"))
            .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite"))
            .stdin(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "isolated redispatch acceptance failed");
        return;
    };
    let auth = root.join("config/cadence/remote-auth");
    login::private_dir(&auth);
    login::private_write(
        &auth.join("trusted-issuer"),
        b"https://issuer.board.localhost",
    );
    login::save_login(&auth, &login::normal_login());
    login::select(&root, "lc");

    let mut png = Vec::new();
    use image::ImageEncoder as _;
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[50, 80, 100], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    use sha2::{Digest, Sha256};
    let digest = format!("{:x}", Sha256::digest(&png));
    let artifact = json!({"ref": format!("med_1473.{digest}"), "digest": digest,
        "bytes": png.len(), "mime": "image/png"});
    let price = json!({"slug":"generate_image","chargeMinor":31_500,"currency":"USD","version":"2026-09-29T00:00:00.000Z"});
    let submits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = submits.clone();
    let pki = root.join("tls");
    login::private_dir(&pki);
    let wire = login::Wire::media(
        &pki,
        Arc::new(move |request| {
            let body = if request == "GET /__platform/media/price/image HTTP/1.1\r\n" {
                json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":price}})
            } else if request == "POST /__platform/media/image HTTP/1.1\r\n" {
                seen.fetch_add(1, Ordering::SeqCst);
                json!({"ok":true,"data":{"job":{"id":"med_1473","kind":"image","status":"succeeded",
                "model":"openai/gpt-image-2.5","provider":"fixture","providerTaskId":null,
                "artifacts":[artifact],"artifactError":null,"usage":{"providerCredits":null},
                "price":price,"repeated":false,"cached":false,"stale":false}}})
            } else if request.starts_with("GET /__platform/media/artifacts/") {
                return ("image/png".into(), png.clone());
            } else {
                panic!("unexpected fixture request: {request}")
            };
            ("application/json".into(), body.to_string().into_bytes())
        }),
    );
    let trust = wire.trust();
    let _trust = crate::issuer_transport::test_fixture::install(trust.clone());
    crate::issue::Pm::init(&root.join("pm")).unwrap();
    let (entered, entries) = mpsc::sync_channel(4);
    let released = Arc::new((Mutex::new(false), Condvar::new()));
    let mut opts = ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", root.join("pm").to_str().unwrap());
    opts.platforms.insert(
        account_media::PLATFORM.into(),
        Arc::new(BlockedProvider {
            inner: account_media::AccountMediaAdapter::new().unwrap(),
            entered,
            released: released.clone(),
        }),
    );
    let state = root.join("tmp/state");
    std::fs::create_dir_all(&state).unwrap();
    let shared = Shared::new(&state, &opts).unwrap();
    let install = install(&shared, &root);
    let approved = approved_run(&shared, &install, "live");
    let id = approved["id"].as_str().unwrap();
    let sender = shared.clone();
    let run_id = id.to_string();
    let dispatch = std::thread::spawn(move || {
        let _trust = crate::issuer_transport::test_fixture::install(trust);
        sender.dispatch_app_run(&run_id)
    });
    entries
        .recv_timeout(Duration::from_secs(10))
        .expect("first provider entry");
    // The monitor must not mistake this real, committed claim for a crash.
    shared.advance_app_runs();
    let during = shared.store.app_run_show(id).unwrap();
    let observed = receipts(&shared, id);
    assert_eq!(during["state"], "running", "{during}");
    assert_eq!(during["steps"][0]["state"], "dispatched", "{during}");
    assert_eq!(observed["claims"], 1, "{observed}");
    assert_eq!(observed["failures"], 0, "{observed}");
    assert_eq!(observed["results"], json!([]), "{observed}");
    assert!(entries.try_recv().is_err(), "second provider entry");
    assert!(shared.host_runs_in_flight.lock().unwrap().contains(id));
    let (lock, ready) = &*released;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    let finished = dispatch.join().unwrap().unwrap();
    assert_eq!(finished["state"], "succeeded", "{finished}");
    let observed = receipts(&shared, id);
    assert_eq!(
        observed["results"].as_array().unwrap().len(),
        1,
        "{observed}"
    );
    assert_eq!(observed["failures"], 0, "{observed}");
    assert_eq!(submits.load(Ordering::SeqCst), 1);
    assert!(entries.try_recv().is_err(), "second provider entry");
    assert!(shared.host_runs_in_flight.lock().unwrap().is_empty());

    // Restart recovery: a persisted dispatched step has no process-local mark.
    // A claim without a receipt must remain uncertain, never send again.
    let approved = approved_run(&shared, &install, "crash-after-claim");
    let id = approved["id"].as_str().unwrap();
    let running = shared
        .store
        .app_run_dispatch(id, approved["snapshot"]["bundle_digest"].as_str().unwrap())
        .unwrap();
    let step = running["snapshot"]["workflow"]["steps"][0]["id"]
        .as_str()
        .unwrap();
    let call_id = format!(
        "app-call-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, format!("{id}:image").as_bytes()).simple()
    );
    assert!(shared
        .store
        .app_capability_claim(crate::store::app_capabilities::AppCapabilityClaim {
            run: id,
            step,
            message: "",
            turn: "",
            slot: "image",
            request: &format!("host-{id}-image"),
            binding_digest: running["snapshot"]["capabilities"]["image"]["digest"]
                .as_str()
                .unwrap(),
            input_digest: &crate::store::app_runs::material_digest(&json!({})),
            call_id: &call_id,
        })
        .unwrap());
    assert!(shared.host_runs_in_flight.lock().unwrap().is_empty());
    let error = shared.dispatch_app_run(id).unwrap_err();
    let uncertain = "A prior provider call has no retained receipt; its outcome is uncertain and no automatic retry was made.";
    assert_eq!(error.kind(), "unknown");
    assert_eq!(error.to_string(), uncertain);
    let failed = shared.store.app_run_show(id).unwrap();
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(failed["failure"]["kind"], "uncertain", "{failed}");
    assert_eq!(failed["failure"]["reason"], uncertain, "{failed}");
    assert_eq!(
        receipts(&shared, id),
        json!({"claims": 1, "failures": 1, "results": []})
    );
    assert_eq!(submits.load(Ordering::SeqCst), 1);
    assert!(
        entries.try_recv().is_err(),
        "uncertain recovery entered provider"
    );
    assert!(shared.host_runs_in_flight.lock().unwrap().is_empty());

    // Crash before the claim is different: that dispatched step can still
    // make its one call, with the provider barrier now released.
    let approved = approved_run(&shared, &install, "crash-before-claim");
    let id = approved["id"].as_str().unwrap();
    shared
        .store
        .app_run_dispatch(id, approved["snapshot"]["bundle_digest"].as_str().unwrap())
        .unwrap();
    let recovered = shared.dispatch_app_run(id).unwrap();
    assert_eq!(recovered["state"], "succeeded", "{recovered}");
    let observed = receipts(&shared, id);
    assert_eq!(observed["claims"], 1);
    assert_eq!(observed["failures"], 0);
    assert_eq!(observed["results"].as_array().unwrap().len(), 1);
    entries.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(entries.try_recv().is_err());
    assert_eq!(submits.load(Ordering::SeqCst), 2);
    assert!(shared.host_runs_in_flight.lock().unwrap().is_empty());
}
