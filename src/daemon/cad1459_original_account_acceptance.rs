//! Independent SEC-003 acceptance. Only account provider HTTP is synthetic;
//! custody, account adapter, Store, run, approval, mount and runtime guards are real.
//! This is a pre-response artifact pause, not CAD-1365's after-header pause.

use super::*;
use crate::issuer_transport::test_fixture;
use crate::remote_cli::cad1459_account_image_acceptance as login;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Condvar};
use std::thread::{self, JoinHandle};

const TEST: &str = "daemon::cad1365_original_transfer_acceptance::cad1459_original_account_acceptance::cad1459_original_account_io_rechecks_before_delivery";
const CHILD: &str = "CAD1459_ORIGINAL_ROOT";

#[derive(Default)]
struct Pause {
    state: Mutex<(bool, bool, bool)>, // armed, reached, released
    changed: Condvar,
}

impl Pause {
    fn arm(&self) {
        *self.state.lock().unwrap() = (true, false, false);
    }

    fn before_response(&self) {
        let mut state = self.state.lock().unwrap();
        if state.0 {
            state.1 = true;
            self.changed.notify_all();
            while !state.2 {
                state = self.changed.wait(state).unwrap();
            }
        }
    }

    fn reached(&self) -> bool {
        let state = self.state.lock().unwrap();
        let (state, _) = self
            .changed
            .wait_timeout_while(state, Duration::from_secs(5), |s| !s.1)
            .unwrap();
        state.1
    }

    fn release(&self) {
        self.state.lock().unwrap().2 = true;
        self.changed.notify_all();
    }
}

// Always release the provider before joining our client/control threads,
// including assertion failures. No paused thread can strand a shared build slot.
struct Pending {
    pause: Arc<Pause>,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.pause.release();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

struct BoardThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for BoardThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Debug)]
struct Observation {
    success: bool,
    bytes: Vec<u8>,
    digest: String,
}

fn original(
    fx: &Fx,
    mode: &str,
    port: u16,
    action: &str,
    token: &str,
    key: &str,
    delivered: &AtomicBool,
) -> Observation {
    match mode {
        "callback" => {
            let mut observation = Observation {
                success: false,
                bytes: Vec::new(),
                digest: String::new(),
            };
            let result = scoped(Asserted::Operator, || {
                fx.shared.rpc_app_media_original_stream(
                    &params(action, token, key, &fx.asset_id),
                    std::process::id(),
                    |header, bytes, _delivery| {
                        delivered.store(true, Ordering::SeqCst);
                        observation.success = true;
                        observation.bytes = bytes.to_vec();
                        observation.digest = header["sha256"].as_str().unwrap().to_owned();
                        Ok(())
                    },
                )
            });
            assert_eq!(
                result.is_ok(),
                observation.success,
                "callback/result disagree"
            );
            observation
        }
        "native" => {
            let (reader, line) = socket_call(
                fx,
                Asserted::Operator,
                "app_media_original",
                params(action, token, key, &fx.asset_id),
            )
            .unwrap();
            let frame: Value = serde_json::from_str(&line).unwrap();
            let success = frame["result"]["transfer"] == "app-media-original/1";
            delivered.store(success, Ordering::SeqCst);
            reader
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            reader
                .take(ORIGINAL_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .unwrap();
            if !success {
                assert!(
                    frame.get("error").is_some(),
                    "ordinary RPC refusal: {frame}"
                );
            }
            Observation {
                success,
                bytes,
                digest: frame["result"]["sha256"].as_str().unwrap_or("").to_owned(),
            }
        }
        "http" => {
            let seam = Seam::token_at(&fx.state).unwrap();
            let (status, ctype, disposition, bytes) = http_original(
                port,
                &seam,
                "operator",
                Some((token, key)),
                json!({"action_token": action, "asset_id": fx.asset_id}),
            );
            let success = status == 200;
            delivered.store(success, Ordering::SeqCst);
            if success {
                assert_eq!(ctype, "image/png");
                assert!(disposition.contains("attachment"));
            } else {
                assert!((400..500).contains(&status), "HTTP refusal status {status}");
                assert!(!ctype.starts_with("image/"));
                assert!(!disposition.contains("attachment"));
                let error: Value =
                    serde_json::from_slice(&bytes).expect("HTTP error JSON, not original bytes");
                assert!(error.is_object());
            }
            Observation {
                success,
                digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
                // An HTTP error envelope is not original binary delivery.
                bytes: if success { bytes } else { Vec::new() },
            }
        }
        _ => unreachable!(),
    }
}

fn inventory(fx: &Fx) -> (usize, usize) {
    let catalog = fx.shared.store.app_media_catalog(20, None).unwrap().0;
    let receipts = fx
        .op("app_run_capability_results", json!({"run_id": fx.run_id}))
        .unwrap();
    (catalog.len(), receipts["results"].as_array().unwrap().len())
}

#[test]
fn cad1459_original_account_io_rechecks_before_delivery() {
    let Some(root) = std::env::var_os(CHILD).map(PathBuf::from) else {
        let root = tempfile::Builder::new()
            .prefix("c1459o-")
            .tempdir()
            .unwrap();
        for name in ["home", "config", "data", "state", "tmp", "locks"] {
            login::private_dir(&root.path().join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1"])
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
        assert!(
            status.success(),
            "isolated account-original acceptance failed"
        );
        return;
    };
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("c1459o-")
    );
    for (key, relative) in [
        ("HOME", "home"),
        ("XDG_CONFIG_HOME", "config"),
        ("TMPDIR", "tmp"),
    ] {
        assert_eq!(
            PathBuf::from(std::env::var_os(key).unwrap()),
            root.join(relative)
        );
    }
    let auth = root.join("config/cadence/remote-auth");
    login::private_dir(&auth);
    login::private_write(
        &auth.join("trusted-issuer"),
        b"https://issuer.board.localhost",
    );
    login::save_login(&auth, &login::normal_login());
    login::select(&root, "lc");

    // One grouped refusal: direct native callback, actual socket headers/body,
    // all three real invalidation axes, and the existing HTTP peer bridge.
    for (case, (mode, axis)) in [
        ("callback", "install"),
        ("native", "mount"),
        ("native", "context"),
        ("http", "install"),
    ]
    .into_iter()
    .enumerate()
    {
        let original_bytes = noisy_square_png();
        let digest = format!("{:x}", Sha256::digest(&original_bytes));
        let artifact = json!({"ref": format!("{JOB_ID}.{digest}"), "digest": digest,
            "bytes": original_bytes.len(), "mime": "image/png"});
        let price = json!({"slug":"generate_image", "chargeMinor":31_500,
            "currency":"USD", "version":"2026-09-29T00:00:00.000Z"});
        let pause = Arc::new(Pause::default());
        let provider_pause = pause.clone();
        let gets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let submits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (provider_gets, provider_submits) = (gets.clone(), submits.clone());
        let provider_bytes = original_bytes.clone();
        let pki = root.join(format!("wire-{case}"));
        login::private_dir(&pki);
        let wire = login::Wire::media(
            &pki,
            Arc::new(move |request| {
                let body = if request == "GET /__platform/media/price/image HTTP/1.1\r\n" {
                    json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":price}})
                } else if request == "POST /__platform/media/image HTTP/1.1\r\n" {
                    provider_submits.fetch_add(1, Ordering::SeqCst);
                    json!({"ok":true,"data":{"job":{"id":JOB_ID,"kind":"image","status":"succeeded",
                    "model":"openai/gpt-image-2.5","provider":"upstream-fixture","providerTaskId":null,
                    "artifacts":[artifact],"artifactError":null,"usage":{"providerCredits":null},
                    "price":price,"repeated":false,"cached":false,"stale":false}}})
                } else if request.starts_with("GET /__platform/media/artifacts/") {
                    provider_gets.fetch_add(1, Ordering::SeqCst);
                    provider_pause.before_response(); // BEFORE even the HTTP status/header
                    return ("image/png".into(), provider_bytes.clone());
                } else {
                    panic!("unexpected synthetic account media operation");
                };
                ("application/json".into(), body.to_string().into_bytes())
            }),
        );
        let trust = wire.trust();
        let _tls = test_fixture::install(trust.clone());
        let fx = Arc::new(Fx::boot_with_account(
            original_bytes,
            None,
            true,
            Some(trust.clone()),
        ));
        let (token, key) = fx.session();
        let (_nonce, action) = fx.mount(&token, &key, 1);
        let (port, board_thread) = if mode == "http" {
            let (port, board) = board(&fx);
            (
                port,
                Some(BoardThread {
                    stop: fx.stop.clone(),
                    thread: Some(board),
                }),
            )
        } else {
            (0, None)
        };
        let before = inventory(&fx);
        assert_eq!(
            before,
            (1, 1),
            "real producer retained one asset and receipt"
        );
        assert_eq!(submits.load(Ordering::SeqCst), 1);
        let delivered = Arc::new(AtomicBool::new(false));
        let positive = original(&fx, mode, port, &action, &token, &key, &delivered);
        assert!(positive.success, "{mode}/{axis}: positive control");
        assert_eq!(
            positive.bytes, fx.original,
            "verified byte-exact account original"
        );
        assert_eq!(positive.digest, format!("sha256:{digest}"));
        assert!(delivered.load(Ordering::SeqCst));
        assert_eq!(inventory(&fx), before);
        assert_eq!(
            submits.load(Ordering::SeqCst),
            1,
            "download did not dispatch"
        );

        let before_gets = gets.load(Ordering::SeqCst);
        delivered.store(false, Ordering::SeqCst);
        pause.arm();
        let mut pending = Pending {
            pause: pause.clone(),
            threads: Vec::new(),
        };
        let (done, reply) = mpsc::channel();
        let (client_fx, client_delivered) = (fx.clone(), delivered.clone());
        let (client_action, client_token, client_key) =
            (action.clone(), token.clone(), key.clone());
        pending.threads.push(thread::spawn(move || {
            let _tls = test_fixture::install(trust);
            let observed = original(
                &client_fx,
                mode,
                port,
                &client_action,
                &client_token,
                &client_key,
                &client_delivered,
            );
            let _ = done.send(observed);
        }));
        assert!(
            pause.reached(),
            "{mode}/{axis}: real artifact GET must reach pre-response pause"
        );
        assert_eq!(gets.load(Ordering::SeqCst), before_gets + 1);
        assert!(
            !delivered.load(Ordering::SeqCst),
            "no header/callback while provider is paused"
        );
        assert!(matches!(reply.try_recv(), Err(mpsc::TryRecvError::Empty)));

        let (controlled, control_reply) = mpsc::channel();
        let control_fx = fx.clone();
        pending.threads.push(thread::spawn(move || {
            // Acquire the REAL PM runtime + custody + release guards, in their
            // production order, while the actual provider GET remains paused.
            let pm = control_fx.shared.pm_at(&control_fx.shared.pm_dir().unwrap()).unwrap();
            crate::issue::app_catalog::workspace::with_runtime_snapshot(&pm, &control_fx.install, |_, _| {
                let _custody = control_fx.shared.platform_custody_lock.lock().unwrap();
                let _release = control_fx.shared.app_release_lock.lock().unwrap();
                // Offline custody discovery must not wait on network custody.
                assert_eq!(crate::platform::account_media::identities().unwrap().len(), 1);
                Ok(())
            }).unwrap();
            let result = match axis {
                "install" => {
                    let shown = control_fx.op("app_workspace_show", json!({"install_id": control_fx.install})).unwrap();
                    let digest = shown["digest"].as_str().or_else(|| shown["install"]["digest"].as_str()).unwrap();
                    let result = control_fx.op("app_local_install_revoke", json!({"install_id": control_fx.install,"digest":digest})).unwrap();
                    assert_eq!(result["approved"], false);
                    assert_eq!(control_fx.shared.store.app_capability_status(&control_fx.install, digest).unwrap()["state"], "revoked");
                    result
                }
                "mount" => {
                    let result = control_fx.op("app_tool_revoke", json!({"action_token":action,"token":token,"key":key,"origin":"loopback"})).unwrap();
                    assert_eq!(result["revoked"], true);
                    result
                }
                "context" => {
                    let (_, proof) = control_fx.shared.store.app_context_proof(&control_fx.install, &control_fx.context).unwrap();
                    let result = control_fx.op("app_context_archive", json!({"install_id":control_fx.install,"context_id":control_fx.context,"expected_revision":proof.revision})).unwrap();
                    assert!(control_fx.shared.store.app_context_proof(&control_fx.install, &control_fx.context).is_err());
                    result
                }
                _ => unreachable!(),
            };
            let _ = controlled.send(result);
        }));
        let control = control_reply.recv_timeout(Duration::from_secs(2));
        let no_early_delivery = !delivered.load(Ordering::SeqCst);
        pause.release();
        // Join even on deadline failure before asserting; no worker remains paused.
        drop(pending);
        assert!(control.is_ok(), "{mode}/{axis}: runtime/custody/release and real revocation must finish within 2s BEFORE provider response");
        assert!(
            no_early_delivery,
            "no success header/callback before response release"
        );
        let refused = reply
            .recv_timeout(Duration::from_secs(5))
            .expect("original request must complete after release");
        assert!(
            !refused.success,
            "{mode}/{axis}: fresh authority must refuse"
        );
        assert!(
            refused.bytes.is_empty(),
            "no original binary bytes after revocation"
        );
        assert!(
            !delivered.load(Ordering::SeqCst),
            "no success header or delivery callback after revocation"
        );
        assert_eq!(
            inventory(&fx),
            before,
            "original reads mint no asset or receipt"
        );
        assert_eq!(submits.load(Ordering::SeqCst), 1, "no second dispatch");
        assert_eq!(
            gets.load(Ordering::SeqCst),
            before_gets + 1,
            "no original retry"
        );
        drop(board_thread);
        drop(fx);
        drop(wire);
    }
}
