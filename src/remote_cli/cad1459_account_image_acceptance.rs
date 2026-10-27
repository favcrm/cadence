//! CAD-1459 TASK-002, independently authored from the ticket and agreed broker API.
//! Exercises the real saved-login custodian and frozen-source guard, not an AOS
//! authorization imitation. TLS supplies only quote bytes; it never decides
//! whether an identity is acceptable. This is not producer/billing acceptance.
//! Run only after coordinator build-slot admission, offline, with isolated temp
//! roots. The self-spawn isolates filesystem environment without clearing any
//! native caller identity. No real saved login is read, even by the parent.

use super::{account_image_identity, account_image_request, AccountImageOperation, RemoteTarget};
use crate::issuer_transport::test_fixture::{self, Fixture};
use serde_json::{json, Value};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TEST: &str =
    "remote_cli::cad1459_account_image_acceptance::selected_normal_login_freezes_image_authority";
const CHILD_ROOT: &str = "CAD1459_ACCEPTANCE_ROOT";
const ORG: &str = "lc";
const WORKSPACE: &str = "ws_cad1459_selected";
const SUBJECT: &str = "user_cad1459_selected";
const BOARD: &str = "https://lc.cadencecloud.app";
const ISSUER: &str = "https://issuer.board.localhost";
const ACCESS: &str = "hct_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const REFRESH: &str = "hcr_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const SESSION: &str = "hcs_12345678-1234-4234-8234-123456789abc";

pub(crate) fn private_write(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

pub(crate) fn private_dir(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

// Normal login's existing private disk schema, CLI-only: no web/provider/media
// capability, platform credential, deployment pin or ingress credential.
pub(crate) fn normal_login() -> Value {
    let now = at();
    json!({
        "version": "hosted-cadence-auth.v1",
        "organization_id": WORKSPACE,
        "organization_slug": ORG,
        "audience": BOARD,
        "issuer": ISSUER,
        "subject_id": SUBJECT,
        "capabilities": ["cli.read", "cli.write"],
        "access_token": ACCESS,
        "expires_at": now + 300,
        "cli_session": {
            "session_id": SESSION,
            "refresh_token": REFRESH,
            "generation": 1,
            "device_label": "CAD-1459 synthetic CLI",
            "idle_expires_at": now + 1800,
            "absolute_expires_at": now + 3600
        },
        "pending_renewal": null
    })
}

pub(crate) fn save_login(dir: &Path, record: &Value) {
    private_write(
        &dir.join(format!("cli-{ORG}.json")),
        &serde_json::to_vec(record).unwrap(),
    );
}

pub(crate) fn select(root: &Path, selected: &str) {
    let registry = json!({
        "selected": {"org": selected},
        "connections": [
            {"selection": {"org": ORG}, "destination": {
                "mode": "remote", "endpoint": BOARD, "org_id": WORKSPACE
            }},
            {"selection": {"org": "local"}, "destination": {
                "mode": "local", "state_dir": root.join("state"),
                "tracker_dir": root.join("pm")
            }},
            {"selection": {"org": "other"}, "destination": {
                "mode": "remote", "endpoint": "https://other.cadencecloud.app",
                "org_id": "ws_cad1459_other"
            }}
        ]
    });
    private_write(
        &root.join("config/cadence/orgs.json"),
        &serde_json::to_vec(&registry).unwrap(),
    );
}

// Disposable local TLS, using only the already-existing thread-local CA/DNS
// fixture. Its resolver denies every nonfixture host. The canonical lc board
// is the fixture's existing alias; the issuer is exclusively synthetic.
type MediaReply = Arc<dyn Fn(&str) -> (String, Vec<u8>) + Send + Sync>;

pub(crate) struct Wire {
    connections: Arc<AtomicUsize>,
    quotes: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    server: Option<JoinHandle<()>>,
    address: SocketAddr,
    ca_der: Vec<u8>,
}

impl Wire {
    fn new(root: &Path) -> Self {
        Self::with_media(root, None)
    }

    #[cfg(feature = "test-seam")]
    pub(crate) fn media(root: &Path, reply: MediaReply) -> Self {
        Self::with_media(root, Some(reply))
    }

    #[cfg(feature = "test-seam")]
    pub(crate) fn connection_count(&self) -> usize {
        self.connections.load(SeqCst)
    }

    #[cfg(feature = "test-seam")]
    pub(crate) fn trust(&self) -> Fixture {
        Fixture {
            ca_der: self.ca_der.clone(),
            loopback: self.address,
        }
    }

    fn with_media(root: &Path, media: Option<MediaReply>) -> Self {
        let pki = root.join("pki");
        private_dir(&pki);
        let openssl = |args: &[&str]| {
            let status = Command::new("openssl")
                .current_dir(&pki)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "disposable TLS fixture creation failed");
        };
        openssl(&[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "2",
            "-subj",
            "/CN=CAD-1459 acceptance CA",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ]);
        openssl(&[
            "req",
            "-new",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "leaf.key",
            "-out",
            "leaf.csr",
            "-subj",
            "/CN=CAD-1459 acceptance server",
        ]);
        private_write(&pki.join("leaf.ext"), b"subjectAltName=DNS:lc.cadencecloud.app,DNS:issuer.board.localhost\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=serverAuth\n");
        openssl(&[
            "x509",
            "-req",
            "-in",
            "leaf.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-days",
            "2",
            "-extfile",
            "leaf.ext",
            "-out",
            "leaf.pem",
        ]);
        for key in ["ca.key", "leaf.key"] {
            fs::set_permissions(pki.join(key), fs::Permissions::from_mode(0o600)).unwrap();
        }
        let ca_der = rustls_pemfile::certs(&mut BufReader::new(
            fs::File::open(pki.join("ca.pem")).unwrap(),
        ))
        .next()
        .unwrap()
        .unwrap()
        .as_ref()
        .to_vec();
        let chain = rustls_pemfile::certs(&mut BufReader::new(
            fs::File::open(pki.join("leaf.pem")).unwrap(),
        ))
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap();
        let key = rustls_pemfile::private_key(&mut BufReader::new(
            fs::File::open(pki.join("leaf.key")).unwrap(),
        ))
        .unwrap()
        .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let connections = Arc::new(AtomicUsize::new(0));
        let quotes = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (hits, accepted, done) = (connections.clone(), quotes.clone(), stop.clone());
        let server = thread::spawn(move || {
            while !done.load(SeqCst) {
                let (tcp, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("isolated TLS accept failed: {e}"),
                };
                // Count even failed TLS/renewal/wrong-origin attempts: a refusal
                // must happen before ANY outgoing operation, not merely a POST.
                hits.fetch_add(1, SeqCst);
                tcp.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                tcp.set_write_timeout(Some(Duration::from_secs(3))).unwrap();
                let conn = rustls::ServerConnection::new(config.clone()).unwrap();
                let mut stream = rustls::StreamOwned::new(conn, tcp);
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                let (mut host, mut bearer) = (String::new(), String::new());
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(':') {
                        if key.eq_ignore_ascii_case("host") {
                            host = value.trim().to_owned();
                        }
                        if key.eq_ignore_ascii_case("authorization") {
                            bearer = value.trim().to_owned();
                        }
                        if key.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse().unwrap();
                        }
                    }
                }
                // The account-original fixture reuses only this synthetic TLS
                // transport. Its provider response never supplies native authority.
                if let Some(reply) = &media {
                    assert!(content_length <= 256 * 1024, "bounded fixture request");
                    let mut body = vec![0; content_length];
                    reader.read_exact(&mut body).unwrap();
                    assert!(host == "lc.cadencecloud.app", "media destination changed");
                    assert!(
                        bearer == format!("Bearer {ACCESS}"),
                        "media custody changed"
                    );
                    let (mime, body) = reply(&request);
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                    stream.write_all(&body).unwrap();
                    stream.flush().unwrap();
                    continue;
                }
                // Assertions never print the credential or full headers.
                assert!(
                    request == "GET /__platform/media/price/image HTTP/1.1\r\n",
                    "unexpected outgoing operation"
                );
                assert!(
                    host == "lc.cadencecloud.app",
                    "quote changed its selected destination"
                );
                assert!(
                    bearer == format!("Bearer {ACCESS}")
                        || bearer == format!("Bearer hct_{}", "C".repeat(43)),
                    "quote did not use the normal login custodian"
                );
                let body = json!({"ok":true,"data":{"kind":"image","model":"openai/gpt-image-2.5","price":{
                    "slug":"generate_image","chargeMinor":50000,"currency":"USD","version":"cad1459-fixture"
                }}}).to_string();
                accepted.fetch_add(1, SeqCst);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                stream.flush().unwrap();
            }
        });
        Self {
            connections,
            quotes,
            stop,
            server: Some(server),
            address,
            ca_der,
        }
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        self.server.take().unwrap().join().unwrap();
    }
}

#[test]
fn selected_normal_login_freezes_image_authority() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        let root = tempfile::Builder::new().prefix("c1459-").tempdir().unwrap();
        for name in ["home", "config", "data", "state", "tmp", "locks"] {
            private_dir(&root.path().join(name));
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--test-threads", "1"])
            .env(CHILD_ROOT, root.path())
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
            "isolated account-image acceptance child failed"
        );
        return;
    };
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("c1459-")
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
    let dir = root.join("config/cadence/remote-auth");
    private_dir(&dir);
    let wire = Wire::new(&root);
    let _trust = test_fixture::install(Fixture {
        ca_der: wire.ca_der.clone(),
        loopback: wire.address,
    });
    private_write(&dir.join("trusted-issuer"), ISSUER.as_bytes());
    let baseline = normal_login();
    save_login(&dir, &baseline);
    select(&root, ORG);
    let target = RemoteTarget {
        identity: None,
        org: ORG.into(),
        endpoint: BOARD.into(),
        org_id: WORKSPACE.into(),
    };
    let frozen = account_image_identity(&target)
        .expect("normal selected CLI login must yield account-image identity");
    assert_eq!(
        wire.connections.load(SeqCst),
        0,
        "fresh local identity must not require new consent or HTTP"
    );
    assert!(
        account_image_request(&frozen, AccountImageOperation::Quote, 3).is_ok(),
        "normal CLI-only login must obtain a quote"
    );
    assert_eq!(wire.quotes.load(SeqCst), 1);

    for default in ["local", "other"] {
        select(&root, default);
        assert!(
            account_image_request(&frozen, AccountImageOperation::Quote, 3).is_ok(),
            "ambient default must not retarget frozen identity"
        );
    }
    assert_eq!(wire.quotes.load(SeqCst), 3);

    // Ordinary credential/generation rotation is NOT principal replacement.
    let mut rotated = baseline.clone();
    rotated["access_token"] = json!(format!("hct_{}", "C".repeat(43)));
    rotated["cli_session"]["refresh_token"] = json!(format!("hcr_{}", "D".repeat(43)));
    rotated["cli_session"]["generation"] = json!(2);
    save_login(&dir, &rotated);
    assert!(
        account_image_request(&frozen, AccountImageOperation::Quote, 3).is_ok(),
        "same-source credential rotation must preserve authority"
    );
    assert_eq!(wire.quotes.load(SeqCst), 4);

    let before = wire.connections.load(SeqCst);
    assert_eq!(before, 4);
    let mut forged_target = target.clone();
    forged_target.org_id = "ws_cad1459_forged".into();
    assert!(
        account_image_identity(&forged_target).is_err(),
        "forged selected workspace must refuse"
    );
    assert_eq!(wire.connections.load(SeqCst), before);

    for (case, pointer, replacement) in [
        (
            "workspace",
            "/organization_id",
            json!("ws_cad1459_replaced"),
        ),
        ("subject", "/subject_id", json!("user_cad1459_replaced")),
        (
            "source session",
            "/cli_session/session_id",
            json!("hcs_87654321-4321-4324-8234-123456789abc"),
        ),
        ("missing lineage", "/cli_session", Value::Null),
        (
            "issuer",
            "/issuer",
            json!("https://replaced.board.localhost"),
        ),
        (
            "audience",
            "/audience",
            json!("https://other.cadencecloud.app"),
        ),
        ("org alias", "/organization_slug", json!("other")),
    ] {
        // Make renewal due too. A replacement must fail before renewal, not
        // just before the eventual media request or after an issuer refusal.
        let mut replaced = baseline.clone();
        *replaced.pointer_mut(pointer).unwrap() = replacement;
        replaced["expires_at"] = json!(at().saturating_sub(1));
        save_login(&dir, &replaced);
        for operation in [
            AccountImageOperation::Quote,
            AccountImageOperation::Submit {
                input: json!({"prompt":"synthetic acceptance only","max_charge_minor":50000}),
                idempotency_key: "00000000-0000-4000-8000-000000000001".into(),
            },
            AccountImageOperation::Job {
                job_id: "job_cad1459_synthetic".into(),
            },
            AccountImageOperation::Artifact {
                reference: "artifact_cad1459_synthetic".into(),
            },
        ] {
            assert!(
                account_image_request(&frozen, operation, 3).is_err(),
                "replaced {case} revived frozen authority"
            );
            assert_eq!(
                wire.connections.load(SeqCst),
                before,
                "replaced {case} sent an outgoing operation before refusal"
            );
        }
    }
    fs::remove_file(dir.join(format!("cli-{ORG}.json"))).unwrap();
    assert!(
        account_image_request(&frozen, AccountImageOperation::Quote, 3).is_err(),
        "logout/absent source must not fall back to another default"
    );
    assert_eq!(wire.connections.load(SeqCst), before);
}
