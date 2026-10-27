//! CAD-1415 runtime QA fixture: the isolated native lifecycle that the HTTP
//! case `http_board_decision_is_no_weaker_than_the_daemon_guard` runs first
//! when `CAD1415_RUNTIME_ROOT` is set. It reuses the sibling module's `Fx`,
//! `ServerGuard` and proposal helpers and adds no mock executor, route, UI
//! or guard. The original HTTP assertions still run after it returns.
//!
//! Disclosed test-seam bootstrap: the operator and master-agent identities
//! are asserted through `test_seam::scoped`, as in the sibling cases. The
//! browser step drives the built UI with a private one-time login nonce kept
//! in a 0600 file. No operator audit approval is recorded or claimed.
//!
//! Inputs (absolute; a missing or invalid value fails hard, no fallback):
//!   CAD1415_RUNTIME_ROOT  fresh directory under /tmp/c1415-13117/qa-runtime
//!   CAD1415_A_SOURCE      extracted, verified crm-0.1.0 directory
//!   CAD1415_B_SOURCE      extracted crm-0.1.1 directory
//!   CAD1415_A_DIGEST      expected native bundle digest of A
//!   CAD1415_B_DIGEST      expected native bundle digest of B
//! Outputs under the root: ready.json, credentials/ (0600, removed on exit),
//! browser-done.json (written by the browser owner), result.json.
use super::{copy_tree, create_input, op_revision, operation_refused, pending};
use super::{Fx, ServerGuard, CREATE, UPDATE};
use crate::client;
use crate::daemon::{serve_with, ServeOptions};
use crate::operator_auth;
use crate::store::app_records::RecordStore;
use crate::test_seam::{scoped, Asserted, Seam};
use crate::ui::{self, ServeOpts};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const ROOT_ENV: &str = "CAD1415_RUNTIME_ROOT";
const QA_BASE: &str = "/tmp/c1415-13117/qa-runtime";
const DONE_WAIT: Duration = Duration::from_secs(180);
/// The operator link nonce expires after 120 seconds, so it is re-minted
/// into the private file every 90 seconds while the browser step is open.
const NONCE_REMINT: Duration = Duration::from_secs(90);
const BROWSER_OP: &str = "browserop";
const HELD_OP: &str = "heldrollbackop";
const UPGRADE_B: &str = "cad1415-upgrade-b";
const ROLLBACK_A: &str = "cad1415-rollback-a";

struct Inputs {
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
    a_digest: String,
    b_digest: String,
}

/// Removes the private credential file on every exit path, including panic.
struct Credential(PathBuf);

impl Drop for Credential {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn inputs(root: &Path) -> Inputs {
    let qa = Path::new(QA_BASE);
    assert!(
        root.is_absolute() && root.starts_with(qa) && root != qa,
        "{ROOT_ENV} must be an absolute path under {QA_BASE}"
    );
    assert!(
        !root.exists() || root.read_dir().unwrap().next().is_none(),
        "{ROOT_ENV} must be a fresh or empty directory"
    );
    let dir = |name: &str| {
        let value =
            std::env::var(name).unwrap_or_else(|_| panic!("{name} is required with {ROOT_ENV}"));
        let path = PathBuf::from(value);
        assert!(
            path.is_absolute() && path.is_dir(),
            "{name} must be an absolute extracted bundle directory"
        );
        path
    };
    let digest = |name: &str| {
        let value =
            std::env::var(name).unwrap_or_else(|_| panic!("{name} is required with {ROOT_ENV}"));
        assert!(!value.trim().is_empty(), "{name} must not be empty");
        value
    };
    Inputs {
        root: root.to_path_buf(),
        a: dir("CAD1415_A_SOURCE"),
        b: dir("CAD1415_B_SOURCE"),
        a_digest: digest("CAD1415_A_DIGEST"),
        b_digest: digest("CAD1415_B_DIGEST"),
    }
}

fn exe_sha256() -> String {
    let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("{key} missing from native response"))
}

fn tool_ids(value: &Value) -> Vec<String> {
    value["actions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}

/// Discovery as a list plus the refusal text if the native read refused.
/// A refusal is recorded, never silently turned into an empty success.
fn listed(result: Result<Value, String>) -> (Vec<String>, String) {
    match result {
        Ok(value) => (tool_ids(&value), String::new()),
        Err(refusal) => (Vec::new(), refusal),
    }
}

fn has_tool(ids: &[String], tool: &str) -> bool {
    ids.iter().any(|id| id.as_str() == tool)
}

/// Operator-asserted call over the daemon socket (the same seam as the
/// sibling board case's `operator_link_mint`).
fn op(state: &Path, method: &str, params: Value) -> Result<Value, String> {
    scoped(Asserted::Operator, || client::rpc(state, method, params))
        .map_err(|e| format!("{method}: {e}"))
}

fn mint_nonce(state: &Path) -> String {
    operator_auth::ensure_secret(state).unwrap();
    let secret = operator_auth::read_secret(state).unwrap();
    let minted = op(
        state,
        "operator_link_mint",
        json!({"secret":secret,"origin":"loopback"}),
    )
    .expect("operator link mint");
    text(&minted, "nonce").to_owned()
}

fn write_private(path: &Path, value: &str) {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(value.as_bytes()).unwrap();
}

fn replace_private(path: &Path, value: &str) {
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    write_private(&tmp, value);
    std::fs::rename(&tmp, path).unwrap();
}

fn write_json_atomic(path: &Path, value: &Value) {
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).unwrap();
    file.write_all(serde_json::to_string_pretty(value).unwrap().as_bytes())
        .unwrap();
    drop(file);
    std::fs::rename(&tmp, path).unwrap();
}

/// Waits for the browser owner's sanitized result, re-minting the private
/// login nonce on schedule. Bounded ordinary IPC polling, not a subagent.
fn await_browser(root: &Path, state: &Path, credential: &Path) -> Value {
    let done = root.join("browser-done.json");
    let start = Instant::now();
    let mut minted = start;
    loop {
        if done.exists() {
            let text = std::fs::read_to_string(&done).unwrap();
            return serde_json::from_str(&text).expect("browser-done.json is not JSON");
        }
        assert!(
            start.elapsed() < DONE_WAIT,
            "browser-done.json not received within 180 seconds"
        );
        if minted.elapsed() >= NONCE_REMINT {
            replace_private(credential, &mint_nonce(state));
            minted = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn start_board(
    state: &Path,
    pm: &Path,
    stop: &Arc<AtomicBool>,
    threads: &mut Vec<std::thread::JoinHandle<()>>,
) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(20);
    let first = (std::process::id() % 80) as u16;
    let mut offset = 0u16;
    loop {
        assert!(
            Instant::now() < deadline,
            "fixture board startup deadline elapsed"
        );
        let port = 3110 + (first + offset) % 80;
        let (ready, rx) = std::sync::mpsc::channel();
        let board_opts = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(ready),
            test_seam: true,
            ..Default::default()
        };
        let (board_state, board_pm) = (state.to_path_buf(), pm.to_path_buf());
        let board =
            std::thread::spawn(move || drop(ui::serve(&board_state, &board_pm, &board_opts)));
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Ok(())) => {
                threads.push(board);
                return port;
            }
            Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                assert!(board.join().is_ok(), "board thread panicked on port {port}");
                offset += 1;
                assert!(offset < 80, "all 80 fixture board candidates busy");
            }
            Ok(Err(kind)) => {
                assert!(board.join().is_ok(), "board thread panicked on port {port}");
                panic!("fixture board startup failed on port {port}: {kind:?}");
            }
            Err(e) => {
                threads.push(board);
                panic!("fixture board startup failed on port {port}: {e}");
            }
        }
    }
}

/// Runs the native lifecycle once. Every step is a real dispatch; the only
/// seam is the asserted caller identity. Any refusal that is not an expected
/// refusal panics, which unwinds through the owned thread joins.
pub(super) fn run(root: &Path) {
    let inputs = inputs(root);
    let exe_before = exe_sha256();
    let qa = Path::new(QA_BASE);
    std::fs::create_dir_all(qa).unwrap();
    std::fs::create_dir_all(&inputs.root).unwrap();
    let dir = tempfile::Builder::new()
        .prefix("rt")
        .tempdir_in(qa)
        .unwrap();

    // Phase 1: A install, B upgrade and native decisions on the seam Shared.
    let a_src = dir.path().join("crm-a-src");
    copy_tree(&inputs.a, &a_src);
    let b_src = dir.path().join("crm-b-src");
    copy_tree(&inputs.b, &b_src);
    let fx = Fx::seeded(dir, &a_src);
    assert_eq!(fx.digest, inputs.a_digest, "native A digest differs");
    let (install, context) = (fx.install.clone(), fx.context.clone());

    let a_show = fx.operator("app_workspace_show", json!({"install_id": install}));
    assert_eq!(text(&a_show, "digest"), inputs.a_digest);
    let (a_tools, a_refusal) = listed(
        fx.call(
            Asserted::Operator,
            "app_assistant_actions_operator",
            json!({"install_id":install,"context_id":context}),
        )
        .map_err(|e| e.to_string()),
    );
    assert!(
        !has_tool(&a_tools, CREATE) && !has_tool(&a_tools, UPDATE),
        "declared CRM tools visible under A"
    );
    let a_listing = if a_refusal.is_empty() {
        "listed"
    } else {
        "refused"
    };
    let t = fx.create_turn("m-rt-a");
    operation_refused(
        fx.invoke(
            "m-rt-a",
            &t,
            CREATE,
            "op-rt-a",
            create_input("cust-rt-a", "Under A"),
        ),
        "A create proposal",
    );
    fx.finish("m-rt-a");

    let upgrade_check = fx.operator(
        "app_workspace_upgrade_check",
        json!({"install_id":install,"source":b_src.to_str().unwrap(),
            "expected_digest":text(&a_show,"digest"),
            "expected_generation":text(&a_show,"catalog_generation")}),
    );
    assert_eq!(text(&upgrade_check, "digest"), inputs.b_digest);
    let upgraded = fx.operator(
        "app_workspace_upgrade",
        json!({"install_id":install,"source":b_src.to_str().unwrap(),
            "expected_digest":text(&a_show,"digest"),
            "expected_generation":text(&a_show,"catalog_generation"),
            "expected_new_digest":inputs.b_digest,"request_id":UPGRADE_B}),
    );
    let b_show = fx.operator("app_workspace_show", json!({"install_id": install}));
    assert_eq!(text(&b_show, "digest"), inputs.b_digest);

    let (b_tools, b_refusal) = listed(
        fx.call(
            Asserted::Operator,
            "app_assistant_actions_operator",
            json!({"install_id":install,"context_id":context}),
        )
        .map_err(|e| e.to_string()),
    );
    assert!(
        has_tool(&b_tools, CREATE) && has_tool(&b_tools, UPDATE),
        "operator discovery missing declared CRM tools under B: {b_refusal}"
    );
    let t = fx.create_turn("m-rt-discover");
    let (agent_tools, agent_refusal) = listed(
        fx.call(
            Asserted::Agent("master".into()),
            "app_assistant_actions",
            json!({"install_id":install,"context_id":context,
                "message":"m-rt-discover","token":t}),
        )
        .map_err(|e| e.to_string()),
    );
    assert!(
        has_tool(&agent_tools, CREATE) && has_tool(&agent_tools, UPDATE),
        "agent discovery missing declared CRM tools under B: {agent_refusal}"
    );
    fx.finish("m-rt-discover");

    // Native operator decisions (allow_once) on agent proposals.
    let t = fx.create_turn("m-rt-create");
    let created_pending = pending(
        fx.invoke(
            "m-rt-create",
            &t,
            CREATE,
            "op-rt-create",
            create_input("cust-new", "Runtime New Customer"),
        ),
        "runtime create proposal",
    );
    let created = fx
        .decide("op-rt-create", "allow_once", op_revision(&created_pending))
        .expect("native allow_once create");
    assert_eq!(created["operation"]["status"], "succeeded");
    fx.finish("m-rt-create");
    assert_eq!(
        fx.customer("cust-new")["profile"]["consent"]["email"],
        "unknown",
        "native create must default consent to unknown"
    );

    let before_update = fx.customer("cust-a");
    let t = fx.create_turn("m-rt-update");
    let updated_pending = pending(
        fx.invoke(
            "m-rt-update",
            &t,
            UPDATE,
            "op-rt-update",
            json!({"record_id":"cust-a","expected_revision":before_update["revision"],
                "phone":"+85250000777","tags":["vip"]}),
        ),
        "runtime update proposal",
    );
    let updated = fx
        .decide("op-rt-update", "allow_once", op_revision(&updated_pending))
        .expect("native allow_once update");
    assert_eq!(updated["operation"]["status"], "succeeded");
    fx.finish("m-rt-update");
    let after_update = fx.customer("cust-a");
    assert_eq!(after_update["profile"]["phone"], "+85250000777");
    assert_eq!(
        after_update["profile"]["consent"],
        before_update["profile"]["consent"]
    );
    assert_eq!(
        after_update["profile"]["display_name"],
        before_update["profile"]["display_name"]
    );
    assert_eq!(
        after_update["profile"]["email"],
        before_update["profile"]["email"]
    );

    // Pending proposals for the browser and for the held rollback decision.
    let t = fx.create_turn("m-browser");
    let conversation = fx
        .call(
            Asserted::Operator,
            "conversation_create",
            json!({"alias":"master","install_id":install,"context_id":context,"general":true}),
        )
        .expect("native conversation for the browser proposal")["conversation"]["id"]
        .as_str()
        .expect("conversation id")
        .to_owned();
    pending(
        fx.invoke(
            "m-browser",
            &t,
            CREATE,
            BROWSER_OP,
            create_input("cust-browser", "Browser Customer"),
        ),
        "browser proposal",
    );
    fx.finish("m-browser");
    let t = fx.create_turn("m-held");
    let held_pending = pending(
        fx.invoke(
            "m-held",
            &t,
            CREATE,
            HELD_OP,
            create_input("cust-held", "Held Rollback Customer"),
        ),
        "held rollback proposal",
    );
    fx.finish("m-held");
    let held_revision = op_revision(&held_pending);
    let cust_new_snapshot = fx.customer("cust-new");

    // Phase 2: drop Shared, reopen the same state via the daemon and board.
    let Fx { dir, shared, .. } = fx;
    drop(shared);
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    let stop = Arc::new(AtomicBool::new(false));
    let opts = ServeOptions {
        test_seam: true,
        stop: Some(stop.clone()),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let daemon_state = state.clone();
    let daemon = std::thread::spawn(move || serve_with(&daemon_state, opts).unwrap());
    let mut servers = ServerGuard {
        stop: stop.clone(),
        threads: vec![daemon],
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
        assert!(Instant::now() < deadline, "daemon fixture did not start");
        std::thread::sleep(Duration::from_millis(50));
    }
    let port = start_board(&state, &pm, &stop, &mut servers.threads);

    let cred_dir = inputs.root.join("credentials");
    std::fs::create_dir_all(&cred_dir).unwrap();
    std::fs::set_permissions(&cred_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let credential = cred_dir.join("operator-login.nonce");
    write_private(&credential, &mint_nonce(&state));
    let _credential_guard = Credential(credential.clone());

    let host = format!("cadence-{port}.localhost:{port}");
    let ready = json!({
        "schema": 1,
        "status": "ready",
        "root": inputs.root,
        "port": port,
        "install_id": install,
        "context_id": context,
        "conversation_id": conversation,
        "browser_operation_id": BROWSER_OP,
        "held_operation_id": HELD_OP,
        "login_url": format!("http://{host}/login"),
        "chat_url": format!(
            "http://{host}/app-installations/{install}?ctx={context}&conversation={conversation}"
        ),
        "credential_file": credential,
        "done_file": inputs.root.join("browser-done.json"),
        "done_wait_secs": 180,
        "login_nonce_ttl_secs": 120,
        "executable_sha256": exe_before,
    });
    write_json_atomic(&inputs.root.join("ready.json"), &ready);

    let done = await_browser(&inputs.root, &state, &credential);
    assert_eq!(done["status"], "done", "browser step did not finish");

    // Backend readback of the browser decision, not the browser's claim.
    let records = RecordStore::open(&state, &install).unwrap();
    assert_eq!(
        records
            .app_assistant_operation_get(BROWSER_OP, &context)
            .unwrap()["status"],
        "succeeded",
        "browser decision was not applied by the native guard"
    );
    let browser_record =
        records.app_record_show(&context, "cust-browser").unwrap()["record"].clone();
    assert_eq!(
        browser_record["profile"]["display_name"],
        "Browser Customer"
    );
    assert_eq!(browser_record["profile"]["consent"]["email"], "unknown");
    assert_eq!(
        records.app_record_show(&context, "cust-new").unwrap()["record"],
        cust_new_snapshot,
        "browser decision changed cust-new"
    );
    assert_eq!(
        records.app_record_show(&context, "cust-a").unwrap()["record"],
        after_update,
        "browser decision changed cust-a"
    );
    assert_eq!(
        records
            .app_assistant_operation_get(HELD_OP, &context)
            .unwrap()["status"],
        "pending_permission",
        "held rollback proposal moved before rollback"
    );

    // Supported rollback to A through the native upgrade, same checks.
    let show_b = op(&state, "app_workspace_show", json!({"install_id":install}))
        .expect("show before rollback");
    let check_a = op(
        &state,
        "app_workspace_upgrade_check",
        json!({"install_id":install,"source":a_src.to_str().unwrap(),
            "expected_digest":text(&show_b,"digest"),
            "expected_generation":text(&show_b,"catalog_generation")}),
    )
    .expect("rollback proposal");
    let rolled = op(
        &state,
        "app_workspace_upgrade",
        json!({"install_id":install,"source":a_src.to_str().unwrap(),
            "expected_digest":text(&show_b,"digest"),
            "expected_generation":text(&show_b,"catalog_generation"),
            "expected_new_digest":text(&check_a,"digest"),"request_id":ROLLBACK_A}),
    );
    if let Err(refusal) = &rolled {
        write_json_atomic(
            &inputs.root.join("result.json"),
            &json!({"schema":1,"status":"rollback_refused","refusal":refusal,
                "executable_sha256_before":exe_before}),
        );
        panic!("native rollback to A refused: {refusal}");
    }
    let show_a = op(&state, "app_workspace_show", json!({"install_id":install}))
        .expect("show after rollback");
    assert_eq!(text(&show_a, "digest"), inputs.a_digest);
    assert_eq!(text(&show_a, "install_id"), install);

    let (tools_after, after_refusal) = listed(op(
        &state,
        "app_assistant_actions_operator",
        json!({"install_id":install,"context_id":context}),
    ));
    assert!(
        !has_tool(&tools_after, CREATE) && !has_tool(&tools_after, UPDATE),
        "declared CRM tools still visible after rollback"
    );
    let after_listing = if after_refusal.is_empty() {
        "listed"
    } else {
        "refused"
    };
    let held_decision = op(
        &state,
        "app_assistant_decision",
        json!({"install_id":install,"context_id":context,"operation_id":HELD_OP,
            "decision":"allow_once","expected_revision":held_revision}),
    );
    assert!(
        held_decision.is_err(),
        "held B decision applied after rollback"
    );
    let records = RecordStore::open(&state, &install).unwrap();
    assert_eq!(
        records.app_record_show(&context, "cust-browser").unwrap()["record"],
        browser_record,
        "rollback changed cust-browser"
    );
    assert_eq!(
        records.app_record_show(&context, "cust-new").unwrap()["record"],
        cust_new_snapshot,
        "rollback changed cust-new"
    );
    assert_eq!(
        records.app_record_show(&context, "cust-a").unwrap()["record"],
        after_update,
        "rollback changed cust-a"
    );
    assert!(
        records.app_record_show(&context, "cust-held").is_err(),
        "held B decision stored cust-held"
    );
    let exe_after = exe_sha256();
    assert_eq!(exe_after, exe_before, "executable changed during the run");

    write_json_atomic(
        &inputs.root.join("result.json"),
        &json!({
            "schema": 1,
            "status": "executed_pending_review",
            "executable_sha256_before": exe_before,
            "executable_sha256_after": exe_after,
            "digests": {"a": inputs.a_digest, "b": inputs.b_digest},
            "install_id": install,
            "context_id": context,
            "conversation_id": conversation,
            "upgrade_b_request": UPGRADE_B,
            "upgrade_b_receipt": upgraded,
            "discovery": {"a": a_listing, "after_rollback": after_listing},
            "rollback_request": ROLLBACK_A,
            "operations": {
                BROWSER_OP: "succeeded",
                HELD_OP: "pending_permission_before_rollback",
                "held_decision_after_rollback": "refused",
            },
            "browser_done_status": done["status"],
            "bootstrap": "test-seam caller identity; isolated development evidence",
            "operator_audit_approval_recorded": false,
        }),
    );
    drop(servers);
}
