//! Independent bad-case acceptance for daemon-owned ticket comments.
//! Included only as a private child module of `daemon` in cfg(test).
use super::*;
use crate::issue::durability::tracker::{AuthorityBinding, HostReceipt, Mode};
use crate::issue::{self, write as issue_write, Pm};
use serde_json::json;
use std::fs;
use std::net::SocketAddr;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Response, Server};

const CHILD: &str = "CAD1180_DAEMON_GUARD_CHILD";
const TEST_NAME: &str = "daemon::cad1180_durability_acceptance::required_daemon_pm_refuses_comment_when_authority_is_unavailable";

/// Keep boot variables out of the main parallel test process. The child is
/// this test binary running only the named scenario, not another application.
#[test]
fn required_daemon_pm_refuses_comment_when_authority_is_unavailable() {
    if std::env::var_os(CHILD).is_some() {
        run_bad_case();
        return;
    }

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("launch isolated acceptance child");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll owned acceptance child") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop timed-out owned acceptance child");
            let _ = child.wait();
            panic!("acceptance child exceeded 30-second bound");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = child
        .wait_with_output()
        .expect("reap owned acceptance child");
    assert!(
        status.success(),
        "isolated daemon durability acceptance failed (status {status}):\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_bad_case() {
    let root = tempfile::Builder::new()
        .prefix("c9dq.")
        .tempdir_in("/tmp")
        .expect("short owned fixture root");
    let pm_root = root.path().join("pm");
    let state = root.path().join("state");
    fs::create_dir_all(&state).unwrap();
    let pm = Pm::init(&pm_root).expect("initialize actual fixture PM");
    issue_write::project_add(&pm, "fixture", "TKT", &[], &[], &[], None).unwrap();
    let created = issue_write::new_issue(
        &pm,
        &pm.dir,
        Some("fixture"),
        "durability guard fixture",
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
    let ticket = created["id"].as_str().unwrap().to_owned();
    let before_revision =
        issue_write::issue_rev(&issue::board::find_issue(&pm_root, &ticket).unwrap().dir).unwrap();
    let before_head = git_head(&pm_root);
    let before_comments = comment_snapshot(&pm_root, &ticket);

    let binding = AuthorityBinding {
        company: "fixture-company".into(),
        instance: "fixture-instance".into(),
        generation: 1,
        authority_epoch: 1,
        boot_id: "fixture-boot".into(),
    };
    let receipt = HostReceipt {
        publication_id: "fixture-publication".into(),
        sequence: 1,
        store_effect_id: "fixture-effect".into(),
        binding: binding.clone(),
        commit: before_head.clone(),
        origin_lease_epoch: None,
        artifact_sha256: "a".repeat(64),
        artifact_bytes: 1,
        manifest_sha256: "b".repeat(64),
    };
    let boot = json!({
        "protocol":"tracker-v1", "mode":"required", "binding":binding,
        "head":receipt, "restore_ready":true,
        "limits":{"artifactBytes":268435456,"chunkBytes":1048576,"chunks":256,
                  "manifestBytes":131072,"ioMs":20000}
    });
    let boot_file = root.path().join("boot.json");
    fs::write(&boot_file, serde_json::to_vec(&boot).unwrap()).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&boot_file, fs::Permissions::from_mode(0o600)).unwrap();
    std::env::set_var("CADENCE_PM_DIR", &pm_root);
    std::env::set_var("CADENCE_TRACKER_BOOT_FILE", &boot_file);
    std::env::set_var("CADENCE_TRACKER_BOOT_ID", "fixture-boot");

    let unavailable = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicUsize::new(0));
    let (address, stop, server_thread) =
        authority_server(binding, receipt, unavailable.clone(), hits.clone());
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_root.to_str().unwrap());
    let shared = crate::issue::durability::tracker::with_loopback_transport(address, || {
        Shared::new(&state, &opts).expect("construct actual Shared through startup resolver")
    })
    .unwrap();

    let retained = shared
        .tracker_durability
        .as_ref()
        .expect("verified tracker context retained by Shared");
    assert_eq!(
        retained.mode,
        Mode::Required,
        "fixture must retain verified Required mode"
    );
    assert!(
        retained.hosted.is_some(),
        "fixture must retain real Hosted adapter"
    );
    assert_eq!(retained.pm_root, fs::canonicalize(&pm_root).unwrap());
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "startup verifier must query the real adapter"
    );

    // The exact context is retained, then the authority's live binding check
    // refuses. The subsequent daemon-created Pm must fail before mutation.
    unavailable.store(true, Ordering::SeqCst);
    let daemon_pm = shared.pm_at(&pm_root).expect("actual daemon Pm factory");
    let outcome = issue_write::add_comment(
        &daemon_pm,
        &ticket,
        "must not be written while required tracker authority is unavailable",
        Some("qa"),
        None,
        Some(&before_revision),
        "cad1180-independent-qa",
    );
    let after_head = git_head(&pm_root);
    let after_revision =
        issue_write::issue_rev(&issue::board::find_issue(&pm_root, &ticket).unwrap().dir).unwrap();
    let after_comments = comment_snapshot(&pm_root, &ticket);
    println!(
        "CAD1180_GUARD_PROOF outcome={outcome:?} head_before={before_head} head_after={after_head} revision_before={before_revision} revision_after={after_revision} comments_unchanged={}",
        before_comments == after_comments
    );
    assert!(
        outcome.is_err(),
        "Required daemon writer must refuse before writing"
    );
    assert_eq!(after_head, before_head, "refusal must preserve Git HEAD");
    assert_eq!(
        after_revision, before_revision,
        "refusal must preserve ticket revision"
    );
    assert_eq!(
        after_comments, before_comments,
        "refusal must preserve ticket comment bytes"
    );
    assert!(
        hits.load(Ordering::SeqCst) >= 2,
        "write guard must consult the unavailable authority"
    );

    stop.send(()).unwrap();
    server_thread.join().unwrap();
}

fn authority_server(
    binding: AuthorityBinding,
    receipt: HostReceipt,
    unavailable: Arc<AtomicBool>,
    hits: Arc<AtomicUsize>,
) -> (SocketAddr, mpsc::Sender<()>, thread::JoinHandle<()>) {
    let server = (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("bind one owned fixture port in 3110..3199");
    let address = server.server_addr().to_ip().expect("IP fixture address");
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("cad1180-owned-authority-fixture".into())
        .spawn(move || loop {
            if stop_rx.try_recv().is_ok() {
                break;
            }
            let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(50)) else {
                continue;
            };
            hits.fetch_add(1, Ordering::SeqCst);
            let (status, body) = if unavailable.load(Ordering::SeqCst) {
                (503, json!({"ok":false,"code":"not_ready"}))
            } else {
                (
                    200,
                    json!({"ok":true,"data":{
                        "binding":binding.clone(), "head":receipt.clone(),
                        "mode":"required", "restore_ready":true
                    }}),
                )
            };
            request
                .respond(Response::from_string(body.to_string()).with_status_code(status))
                .unwrap();
        })
        .unwrap();
    (address, stop_tx, handle)
}

fn git_head(root: &std::path::Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn comment_snapshot(root: &std::path::Path, ticket: &str) -> Vec<(String, Vec<u8>)> {
    let dir = issue::board::find_issue(root, ticket)
        .unwrap()
        .dir
        .join("comments");
    if !dir.exists() {
        return Vec::new();
    }
    let mut files = fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}
