//! Independent QA control peer, not an oracle or a Store substitute.
//! Included ONLY as a cfg(test) child of ui::cli_route. No production export.
use super::*;
use crate::issue::durability::{self, observation, tracker};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, Read, Write};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

const PREFIX: &str = "CAD1180_PEER ";
fn emit(value: Value) {
    // One serialized line, no test-harness text can masquerade as an event.
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{PREFIX}{value}").unwrap();
    stdout.flush().unwrap();
}
fn string(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap().to_owned()
}
fn endpoint() -> SocketAddr {
    let address: SocketAddr = std::env::var("QA_NATIVE_TRANSPORT")
        .unwrap()
        .parse()
        .unwrap();
    tracker::with_loopback_transport(address, || ()).unwrap();
    address
}
struct Capture {
    label: String,
    spool: PathBuf,
}
impl observation::Observer for Capture {
    fn captured(&self, receipt: &durability::Receipt, artifact: &[u8]) {
        // The supplied bytes are copied only into a QA-owned spool, under the
        // original guard at the native callback, BEFORE the real HTTP reserve.
        let path = self.spool.join(format!("{}.bundle", self.label));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(artifact).unwrap();
        file.sync_all().unwrap();
        emit(json!({"event":"captured", "label":self.label,
            "commit":receipt.commit, "bundlePath":path, "origin":receipt.json()}));
    }
    fn reserved(&self, reservation: &durability::Reservation) {
        // This is the native accepted token AFTER all adapter checks, not the
        // host's raw reserve response, and not a separately ordered request.
        emit(json!({"event":"reservation", "label":self.label,
            "receipt":reservation.receipt, "expires_at":reservation.expires_at}));
    }
}
fn initialize(pm_dir: &Path) -> String {
    let pm = Pm::at(pm_dir).unwrap();
    issue_write::project_add(&pm, "fixture", "TKT", &[], &[], &[], None).unwrap();
    let created = issue_write::new_issue(
        &pm,
        &pm.dir,
        Some("fixture"),
        "original",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "qa-seed",
    )
    .unwrap();
    let pad = pm.dir.join("fixtures/qa-pad.bin");
    let lock = pm.lock().unwrap();
    fs::create_dir_all(pad.parent().unwrap()).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pad)
        .unwrap();
    let mut remaining = 1_048_833;
    let mut ordinal = 0;
    while remaining > 0 {
        let block = Sha256::digest(format!("aos174-pad/{ordinal}").as_bytes());
        let n = remaining.min(block.len());
        file.write_all(&block[..n]).unwrap();
        remaining -= n;
        ordinal += 1;
    }
    file.sync_all().unwrap();
    pm.commit_scoped(&lock, &[pad], "QA inert initial bundle pad")
        .unwrap();
    drop(lock);
    string(&created, "id")
}
fn route(command: Value, config: Value, missing: bool) {
    let label = string(&command, "label");
    let pm_dir = PathBuf::from(string(&config, "pm"));
    let state = PathBuf::from(string(&config, "state"));
    let ticket = string(&command, "ticketId");
    let change = &command["change"];
    let issue = board::find_issue(&pm_dir, &ticket).unwrap();
    // Normal revision from the actual current issue, not the accepted host HEAD.
    let revision = issue_write::issue_rev(&issue.dir).unwrap();
    let (verb, args) = if let Some(title) = change["title"].as_str() {
        (
            "issue_set",
            json!({"ids":[ticket],"set":[format!("title={title}")],"if_rev":revision}),
        )
    } else {
        (
            "issue_comment",
            json!({"id":ticket,"body":change["comment"].as_str().unwrap(),"if_rev":revision}),
        )
    };
    let bytes = serde_json::to_string(&args).unwrap();
    emit(json!({"event":"request", "label":label, "verb":verb,
        "body_sha256":format!("{:x}",Sha256::digest(bytes.as_bytes())), "arguments":args}));
    // tiny_http's request builder supplies only incoming HTTP syntax. Actual
    // post independently verifies signed issuer/sub/company/scope/time/jti and
    // reads these exact bytes. No response, actor context or Store is mocked.
    // <=5 bounded request bodies retained until the owned peer exits.
    let body: &'static str = Box::leak(bytes.into_boxed_str());
    let mut request: Request = tiny_http::TestRequest::new()
        .with_method(Method::Post)
        .with_path(&format!("/api/cli/{verb}"))
        .with_body(body)
        .with_header(Header::from_bytes("Host", "cad1180-peer.board.localhost").unwrap())
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(
            Header::from_bytes(
                "Authorization",
                format!("Bearer wikienv_{}", string(&command, "envelope")),
            )
            .unwrap(),
        )
        .into();
    let opts = ServeOpts {
        public: Some(crate::ui::PublicBoard {
            host: "cad1180-peer.board.localhost".into(),
            issuer: string(&config, "issuer"),
            company: string(&config, "company"),
            authorize_url: format!("{}/authorize", string(&config, "issuer")),
        }),
        // Regular calls get their real adapter from runtime_from_env INSIDE
        // post; missing calls have no boot env and independently Required None.
        durability_mode: durability::Mode::Required,
        durability: None,
        ..ServeOpts::default()
    };
    if missing {
        assert!(std::env::var_os("CADENCE_TRACKER_BOOT_FILE").is_none());
        assert!(std::env::var_os("CADENCE_TRACKER_BOOT_ID").is_none());
    }
    let observer = Arc::new(Capture {
        label: label.clone(),
        spool: PathBuf::from(string(&config, "spool")),
    });
    // Install BOTH RAII contexts on THIS handler thread. post is synchronous;
    // there is no async poll/spawn inside the scoped closure. A/B run on their
    // own handler threads; neither thread inherits the other's observer.
    let response = tracker::with_loopback_transport(endpoint(), || {
        observation::with_observer(observer, || {
            post(&mut request, &Method::Post, verb, &state, &pm_dir, &opts)
        })
    })
    .unwrap();
    let status = response.status_code().0;
    let mut raw = Vec::new();
    response.into_reader().read_to_end(&mut raw).unwrap();
    let body: Value = serde_json::from_slice(&raw).unwrap();
    emit(json!({"event":"done", "label":label, "status":status, "body":body}));
}

// Control entry, NOT an acceptance test. With no explicit QA invocation it has
// no effects and establishes no PASS. The independent TS oracle is the check.
#[test]
fn cad1180_native_peer_json() {
    let Ok(mode) = std::env::var("QA_NATIVE_PEER_MODE") else {
        return;
    };
    if mode == "verify" {
        let file = PathBuf::from(std::env::var("QA_NATIVE_BOOT_FILE").unwrap());
        let result =
            tracker::with_loopback_transport(endpoint(), || durability::verify_boot_command(&file))
                .unwrap();
        match result {
            Ok(value) => emit(json!({"event":"boot-verified", "result":value})),
            Err(error) => {
                emit(json!({"event":"boot-refused", "error":error.to_string()}));
                panic!("actual verify_boot_command refused: {error}");
            }
        }
        return;
    }
    assert!(mode == "json" || mode == "missing");
    let config: Value =
        serde_json::from_slice(&fs::read(std::env::var("QA_NATIVE_PEER_CONFIG").unwrap()).unwrap())
            .unwrap();
    assert_eq!(string(&config, "pm"), "/root/pm");
    assert_eq!(std::env::var("CADENCE_PM_DIR").unwrap(), "/root/pm");
    assert!(string(&config, "state").starts_with("/root/"));
    assert!(string(&config, "spool").starts_with("/root/"));
    let mut handlers = Vec::new();
    let mut shutdown = false;
    let mut initialized = mode == "missing";
    let mut labels = std::collections::BTreeSet::new();
    emit(json!({"event":"ready"}));
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        assert!(line.len() <= 16_384);
        let command: Value = serde_json::from_str(&line).unwrap();
        match command["op"].as_str().unwrap() {
            "initialize" => {
                assert!(!initialized && handlers.is_empty() && mode == "json");
                let id = initialize(Path::new(&string(&config, "pm")));
                initialized = true;
                emit(json!({"event":"initialized", "ticketId":id}));
            }
            "write" => {
                assert!(initialized);
                let label = string(&command, "label");
                assert!(matches!(label.as_str(), "A" | "B" | "C" | "D" | "missing"));
                assert_eq!(label == "missing", mode == "missing");
                assert!(labels.insert(label.clone()));
                let copy = config.clone();
                let missing = mode == "missing";
                handlers.push(thread::spawn(move || {
                    if std::panic::catch_unwind(|| route(command, copy, missing)).is_err() {
                        emit(json!({"event":"fault", "label":label, "error":"actual native handler panicked"}));
                        panic!("native handler failed");
                    }
                }));
            }
            "shutdown" => {
                shutdown = true;
                break;
            }
            _ => panic!("unknown QA control command"),
        }
    }
    for handler in handlers {
        handler.join().unwrap();
    }
    assert!(shutdown, "control channel closed without explicit drain");
    emit(json!({"event":"closed"}));
}
