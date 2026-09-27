//! CAD688 actual peer authority and credential incarnation lifecycle.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::contract_fixture::FakePlatform;
use cadence_agent::platform::PlatformAdapter;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

fn fixture() -> TestDaemon {
    let mut opts = daemon_opts();
    opts.test_seam = false;
    opts.platforms
        .insert("fixture".into(), Arc::new(FakePlatform::standard()));
    TestDaemon::start_opts(opts)
}
fn create(account: &str, token: &str) -> Value {
    json!({"provider":"fixture","account":account,"shape":"token","token":token,"scopes":["widgets:read"],"accept_same_uid_risk":true})
}
fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    // Build literal two-field JSON, avoiding proto::request's optional test seam.
    let frame = json!({"method":method,"params":params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane.dir.path().join(format!("native-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

#[test]
fn cad688_native_and_setsid_connection_management_is_operator_only() {
    let d = fixture();
    let created = d
        .operator_rpc(
            "connection_create",
            create("native-account", "cadp_conn_secret_12345"),
        )
        .unwrap();
    let id = created["connection"]["id"].as_str().unwrap();
    assert_eq!(
        d.operator_rpc("connection_show", json!({"connection_id":id}))
            .unwrap(),
        created
    );
    let mut lane = LaneShell::spawn(d.dir.path());
    plant_member_pane(&d, "connection-peer", "claude", None, lane.pid());
    let before = d.operator_rpc("connection_list", json!({})).unwrap();
    let calls = [
        ("connection_providers", json!({})),
        ("connection_list", json!({})),
        ("connection_show", json!({"connection_id":id})),
        ("connection_check", json!({"connection_id":id})),
        (
            "connection_create",
            create("forged-account", "cadp_conn_other_secret"),
        ),
        (
            "connection_rotate",
            json!({"connection_id":id,"token":"cadp_conn_rotate_secret"}),
        ),
        ("connection_revoke", json!({"connection_id":id})),
    ];
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &calls {
            let frame = native(&mut lane, &d.state, detached, method, params.clone());
            if frame["ok"] != false
                || frame.get("result").is_some()
                || !frame["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("operator action")
            {
                failures.push(json!({"detached":detached,"method":method,"frame":frame}));
            }
        }
        let frame = native(
            &mut lane,
            &d.state,
            detached,
            "connection_revoke",
            json!({"connection_id":id,"agent":"operator","provider":"fixture","account":"native-account"}),
        );
        assert_eq!(frame["ok"], false);
    }
    assert!(
        failures.is_empty(),
        "actual peer connection admission failed: {failures:?}"
    );
    assert_eq!(
        d.operator_rpc("connection_list", json!({})).unwrap(),
        before
    );
}
#[test]
fn cad688_rotation_preserves_id_and_stale_id_cannot_address_reenrollment() {
    let d = fixture();
    let first = d
        .operator_rpc(
            "connection_create",
            create("lifecycle", "cadp_conn_first_secret"),
        )
        .unwrap();
    let id = first["connection"]["id"].clone();
    let rotated = d
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id":id,"token":"cadp_conn_second_secret"}),
        )
        .unwrap();
    assert_eq!(rotated["connection"]["id"], id);
    assert_eq!(rotated["connection"]["revision"], 2);
    d.operator_rpc("connection_revoke", json!({"connection_id":id}))
        .unwrap();
    let next = d
        .operator_rpc(
            "connection_create",
            create("lifecycle", "cadp_conn_third_secret"),
        )
        .unwrap();
    assert_ne!(next["connection"]["id"], id);
    for method in [
        "connection_show",
        "connection_check",
        "connection_rotate",
        "connection_revoke",
    ] {
        let mut params = json!({"connection_id":id});
        if method == "connection_rotate" {
            params["token"] = json!("cadp_stale_secret");
        }
        assert!(
            d.operator_rpc(method, params).is_err(),
            "stale ID admitted {method}"
        );
    }
    assert_eq!(
        d.operator_rpc(
            "connection_show",
            json!({"connection_id":next["connection"]["id"]})
        )
        .unwrap(),
        next
    );
    let db = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM platform_grants", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn cad688_concurrent_rotate_revoke_cannot_retarget_a_replacement() {
    let d = fixture();
    let first = d
        .operator_rpc(
            "connection_create",
            create("concurrent", "cadp_concurrent_first"),
        )
        .unwrap();
    let id = first["connection"]["id"].clone();
    std::thread::scope(|scope| {
        let rotate = scope.spawn(|| {
            d.operator_rpc(
                "connection_rotate",
                json!({"connection_id":id,"token":"cadp_concurrent_second"}),
            )
        });
        let revoke =
            scope.spawn(|| d.operator_rpc("connection_revoke", json!({"connection_id":id})));
        let _ = rotate.join().unwrap();
        revoke.join().unwrap().unwrap();
    });
    let next = d
        .operator_rpc(
            "connection_create",
            create("concurrent", "cadp_concurrent_third"),
        )
        .unwrap();
    assert_ne!(next["connection"]["id"], id);
    assert!(d
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id":id,"token":"cadp_concurrent_stale"})
        )
        .is_err());
    assert_eq!(
        d.operator_rpc(
            "connection_show",
            json!({"connection_id":next["connection"]["id"]})
        )
        .unwrap(),
        next
    );
}

struct PausedDescriptor {
    inner: FakePlatform,
    pause: std::sync::atomic::AtomicBool,
    entered: std::sync::mpsc::Sender<()>,
    release: (std::sync::Mutex<bool>, std::sync::Condvar),
}
impl cadence_agent::platform::PlatformAdapter for PausedDescriptor {
    fn table(&self) -> &cadence_agent::contract_fixture::ToolTable {
        self.inner.table()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.inner.reported_manifest_version()
    }
    fn connection_descriptor(
        &self,
    ) -> Option<cadence_agent::platform::connections::ProviderDescriptor> {
        if self.pause.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.entered.send(()).unwrap();
            let (lock, cv) = &self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = cv.wait(released).unwrap();
            }
        }
        self.inner.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.inner.connection_registration()
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
    fn read_back(&self, tool: &str, input: &Value) -> cadence_agent::contract_fixture::Verified {
        self.inner.read_back(tool, input)
    }
    fn source_hash(&self, agent: &str, source: &str) -> Option<String> {
        self.inner.source_hash(agent, source)
    }
}
struct ResumeDescriptor(Arc<PausedDescriptor>);
impl Drop for ResumeDescriptor {
    fn drop(&mut self) {
        *self.0.release.0.lock().unwrap() = true;
        self.0.release.1.notify_all();
    }
}
#[test]
fn cad688_resolved_old_id_cannot_rotate_reenrolled_account() {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let adapter = Arc::new(PausedDescriptor {
        inner: FakePlatform::standard(),
        pause: std::sync::atomic::AtomicBool::new(false),
        entered: entered_tx,
        release: (std::sync::Mutex::new(false), std::sync::Condvar::new()),
    });
    let mut opts = daemon_opts();
    opts.test_seam = false;
    opts.platforms.insert("fixture".into(), adapter.clone());
    let d = TestDaemon::start_opts(opts);
    let first = d
        .operator_rpc(
            "connection_create",
            create("paused-account", "cadp_paused_original"),
        )
        .unwrap();
    let id = first["connection"]["id"].clone();
    adapter
        .pause
        .store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::scope(|scope| {
        let _release = ResumeDescriptor(adapter.clone());
        let rotate = scope.spawn(|| {
            d.operator_rpc(
                "connection_rotate",
                json!({"connection_id":id,"token":"cadp_paused_stale"}),
            )
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(15))
            .unwrap();
        d.operator_rpc("connection_revoke", json!({"connection_id":id}))
            .unwrap();
        let replacement = d
            .operator_rpc(
                "connection_create",
                create("paused-account", "cadp_paused_replacement"),
            )
            .unwrap();
        assert_ne!(replacement["connection"]["id"], id);
        *adapter.release.0.lock().unwrap() = true;
        adapter.release.1.notify_all();
        assert!(rotate.join().unwrap().is_err());
        assert_eq!(
            d.operator_rpc(
                "connection_show",
                json!({"connection_id":replacement["connection"]["id"]})
            )
            .unwrap(),
            replacement,
            "stale resolved ID changed replacement metadata"
        );
        let custody = cadence_agent::platform::Custody::open(&d.state).unwrap();
        assert_eq!(
            custody
                .load(
                    "file",
                    &cadence_agent::platform::Key {
                        platform: "fixture",
                        account: "paused-account"
                    }
                )
                .unwrap(),
            b"cadp_paused_replacement",
            "stale resolved ID changed replacement bytes"
        );
    });
}

#[test]
fn cad688_credential_text_cannot_be_persisted_as_public_account_metadata() {
    let d = fixture();
    let before = d.operator_rpc("connection_list", json!({})).unwrap();
    const TOKEN: &str = "cadp_metadata_private_token";
    assert!(d
        .operator_rpc("connection_create", create(TOKEN, TOKEN))
        .is_err());
    assert_eq!(
        d.operator_rpc("connection_list", json!({})).unwrap(),
        before
    );
    let db = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    let events: String = db
        .prepare("SELECT payload FROM events")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(!events.contains(TOKEN));
}
