//! CAD-1128 r2 source-backed regression leaf — independent acceptance author.
//!
//! NOT production code and NOT a frozen driver. Three bounded checks for the
//! reviewer's `agent://VaultSpecSecurityReview/findings` gaps, each against
//! the REAL daemon/RPC, REAL SQLite core, REAL AEAD vault rows and REAL
//! ephemeral TLS SMTP. The manager mounts this leaf and runs it before/after
//! its production fixes; a setup/build failure is not a RED finding.
//!
//! GAPS COVERED (contract r2 lines 23–27, 63, 69):
//! (a) legacy `platform_rotate` on a vault-backed SMTP record must not accept
//!     a token that decodes to the nine-field SMTP custody JSON (Unicode
//!     escapes defeat incidental fragment screens) and must refuse BEFORE any
//!     vault row or core mutation; a proper typed encrypted rotation succeeds.
//! (b) exact-ID/revision secret-only `connection_rotate` must INHERIT the
//!     encrypted transport/sender (not force a full-field fallback), and the
//!     rotated secret must actually be used over TLS after an explicit link
//!     rebind; stale expectations must not write.
//! (c) admitted encrypted test-send must not put server-controlled reply text
//!     in the public receipt/error, while a transmitted-but-unanswered send
//!     stays `unknown` and is never retried automatically.
//!
//! LIMITS: synthetic credentials/marker text only; no provider mock, no
//! production debug API and no new bypass. The SMTP peer is a real TLS server
//! using the fixture's ephemeral CA; the canary is non-secret text. Paths are
//! isolated under one retained `tempfile` root. This author did not build,
//! run, lint or format anything.

#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("r2 gap regressions require admitted Linux test-seam execution; never skip");

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::Rig;
use crate::error::{Error, Result};

const A: &str = "YAwS5AyAesmZzdAuOBbBykufIyVNQQunwIqfTTWJlu4";
const B: &str = "Lc6BitPRSkgpooTCxJPhIuQaLtngFLkWw5MpFl5l4HY";
const B2: &str = "eJ0oM4Ql9jW8uY3wT1sV6xZ2aC5bD7fG0hK9mN4pR8tU";
const ACCOUNT: &str = "r2-gap";
/// Non-secret provider text that must never reach a public receipt/error.
const CANARY: &str = "SYNTH-R2-PROVIDER-ERROR-CANARY-7f3a";
const WAIT: Duration = Duration::from_secs(30);
const PEER_CLEANUP: Duration = Duration::from_secs(15);

fn must<T, E>(result: std::result::Result<T, E>, stage: &'static str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, WAIT)
    })
}

fn start() -> (tempfile::TempDir, Rig) {
    let root = tempfile::Builder::new()
        .prefix("cad1128-r2-gap-")
        .tempdir()
        .expect("fixture root");
    let rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "encrypted rig start",
    );
    (root, rig)
}

fn unlock(rig: &Rig) {
    must(
        rpc(
            rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
        ),
        "explicit unlock failed",
    );
}

/// The rig configures custody but does not create the vault; initialize then
/// unlock through the real operator verbs.
fn initialize(rig: &Rig) {
    must(
        rpc(
            rig,
            "connection_custody_initialize",
            json!({"accept_same_uid_risk":true}),
        ),
        "explicit vault initialize failed",
    );
    unlock(rig);
}

fn create(rig: &Rig, account: &str, secret: &str, port: u16) -> String {
    let created = must(
        rpc(
            rig,
            "connection_create",
            json!({"provider":"smtp","account":account,"shape":"smtp",
                "host":"localhost","port":port,"tls_mode":"implicit",
                "username":"fixture-user","secret":secret,
                "sender":"sender@example.test","scopes":["email:send"],
                "accept_same_uid_risk":true}),
        ),
        "encrypted SMTP create failed",
    );
    created["connection"]["id"]
        .as_str()
        .expect("created connection id")
        .to_string()
}

fn bind(rig: &Rig, id: &str, request: &str) {
    must(
        rpc(
            rig,
            "crm_smtp_bind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,
                "connection_id":id,"request_id":request}),
        ),
        "CRM SMTP bind failed",
    );
}

fn test_send(rig: &Rig) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_test_send",
        json!({"install_id":rig.install_id,"context_id":rig.context_id,
            "campaign_id":rig.campaign_id,"to_email":"recipient@example.test"}),
    )
}

/// Every string in the canonical nine-field SMTP custody document is emitted
/// as JSON `\uXXXX` escapes: the token stays whitespace-free, decodes to the
/// same bytes, and carries no literal secret fragment for an incidental
/// text screen to match.
fn escape_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() * 6 + 2);
    out.push('"');
    for unit in value.encode_utf16() {
        out.push_str(&format!("\\u{:04x}", unit));
    }
    out.push('"');
    out
}

fn escaped_smtp_token(
    host: &str,
    port: u16,
    tls_mode: &str,
    username: &str,
    secret: &str,
    sender: &str,
    sender_name: &str,
) -> String {
    format!(
        "{{\"schema\":1,\"provider\":{},\"host\":{},\"port\":{},\"tls_mode\":{},\
         \"username\":{},\"secret\":{},\"sender\":{},\"sender_name\":{}}}",
        escape_json_string("smtp"),
        escape_json_string(host),
        port,
        escape_json_string(tls_mode),
        escape_json_string(username),
        escape_json_string(secret),
        escape_json_string(sender),
        escape_json_string(sender_name),
    )
}

fn typed_rotation(rig: &Rig, id: &str, revision: u64, secret: &str) -> Value {
    json!({"connection_id":id,"expected_connection_id":id,"expected_revision":revision,
        "secret":secret,"accept_same_uid_risk":true,"host":"localhost",
        "port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user",
        "sender":"sender@example.test"})
}

/// (a) + typed positive.
#[test]
fn legacy_token_shape_rotation_refuses_before_write_and_typed_rotation_succeeds() {
    let (_root, rig) = start();
    initialize(&rig);
    let id = create(&rig, ACCOUNT, A, rig.smtp_port);
    let core_before = must(rig.core_record(&id), "core read").to_json();
    let rows_before = must(rig.vault_rows(&id), "vault row read").len();

    let token = escaped_smtp_token(
        "localhost",
        rig.smtp_port,
        "implicit",
        "fixture-user",
        B,
        "sender@example.test",
        "",
    );
    let error = rpc(
        &rig,
        "platform_rotate",
        json!({"platform":"smtp","account":ACCOUNT,"token":token}),
    )
    .expect_err("legacy token-shape rotation of an encrypted SMTP record must refuse");
    assert!(
        matches!(
            error.code(),
            Some("unsupported_custody_use" | "stale_revision")
        ),
        "refusal was not the shape/expectation admission: {:?}",
        error.code()
    );
    assert_eq!(
        must(rig.core_record(&id), "core read").to_json(),
        core_before,
        "refused legacy rotation changed core authority"
    );
    assert_eq!(
        must(rig.vault_rows(&id), "vault row read").len(),
        rows_before,
        "refused legacy rotation wrote a vault row"
    );

    // Eligible typed encrypted rotation: same secret B, exact caller CAS.
    let rotated = must(
        rpc(&rig, "connection_rotate", typed_rotation(&rig, &id, 1, B)),
        "typed encrypted rotation failed",
    );
    assert_eq!(rotated["connection"]["id"], id);
    let core = must(rig.core_record(&id), "core read");
    assert_eq!(core.credential_revision, 2);
    assert_eq!(core.exchange, "smtp");
    assert!(
        core.custody.starts_with("encrypted-sqlite-v1:"),
        "typed rotation did not publish a vault reference"
    );
    let rows = must(rig.vault_rows(&id), "vault row read");
    assert_eq!(rows.len(), rows_before + 1);
    let latest = rows
        .iter()
        .find(|row| row.credential_revision == 2)
        .expect("revision-2 vault row");
    let envelope = must(
        rig.read_row_using_its_stored_identity(latest),
        "decrypt typed rotation envelope",
    );
    assert_eq!(envelope.host, "localhost");
    assert_eq!(envelope.port, rig.smtp_port);
    assert_eq!(envelope.tls_mode, "implicit");
    assert_eq!(envelope.username, "fixture-user");
    assert_eq!(envelope.sender, "sender@example.test");
    assert_eq!(envelope.secret(), B.as_bytes());
    must(rig.finish(), "rig finish");
}

/// (b) secret-only inheritance, explicit rebind, B use, stale no-write.
#[test]
fn encrypted_secret_only_rotation_inherits_and_b_use_after_rebind() {
    let (_root, rig) = start();
    initialize(&rig);
    let id = create(&rig, ACCOUNT, A, rig.smtp_port);
    bind(&rig, &id, "r2-inherit-bind");
    // The link's own revision does not change on a credential rotation; the
    // board rebinds with this exact value after the rotate.
    let live = must(
        rpc(
            &rig,
            "crm_smtp_show",
            json!({"install_id":rig.install_id,"context_id":rig.context_id}),
        ),
        "CRM SMTP show before rotation failed",
    );
    let link_revision = live["binding"]["link_revision"]
        .as_i64()
        .expect("link revision");

    // NO host/port/tls_mode/username/sender and NO shape: only the fresh
    // secret plus the exact caller expectations.
    let rotated = must(
        rpc(
            &rig,
            "connection_rotate",
            json!({"connection_id":id,"expected_connection_id":id,
                "expected_revision":1,"secret":B}),
        ),
        "secret-only encrypted rotation refused the supported inheritance",
    );
    assert_eq!(rotated["connection"]["id"], id);
    let core = must(rig.core_record(&id), "core read");
    assert_eq!(core.credential_revision, 2);
    assert_eq!(core.exchange, "smtp");
    let rows = must(rig.vault_rows(&id), "vault row read");
    let latest = rows
        .iter()
        .find(|row| row.credential_revision == 2)
        .expect("revision-2 vault row");
    let envelope = must(
        rig.read_row_using_its_stored_identity(latest),
        "decrypt inherited envelope",
    );
    assert_eq!(
        (
            envelope.host.as_str(),
            envelope.port,
            envelope.tls_mode.as_str(),
            envelope.username.as_str(),
            envelope.sender.as_str(),
        ),
        (
            "localhost",
            rig.smtp_port,
            "implicit",
            "fixture-user",
            "sender@example.test"
        ),
        "secret-only rotation did not inherit the saved transport/sender"
    );
    assert_eq!(envelope.secret(), B.as_bytes());

    // The link is pinned to revision 1: explicit rebind, then use B for real.
    must(
        rpc(
            &rig,
            "crm_smtp_rebind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,
                "connection_id":id,"expected_revision":link_revision}),
        ),
        "CRM SMTP rebind failed",
    );
    let before = must(rig.smtp_barrier(), "pre-send barrier");
    must(test_send(&rig), "test-send with the rotated secret failed");
    let after = must(rig.smtp_barrier(), "post-send barrier");
    assert_eq!(after.auth_b, before.auth_b + 1, "B was not used");
    assert_eq!(
        after.auth_a, before.auth_a,
        "the replaced A secret was used"
    );
    assert_eq!(after.accepted_messages, before.accepted_messages + 1);

    // Stale caller expectations must not write.
    let core_before = must(rig.core_record(&id), "core read").to_json();
    let rows_before = must(rig.vault_rows(&id), "vault row read").len();
    let error = rpc(
        &rig,
        "connection_rotate",
        json!({"connection_id":id,"expected_connection_id":id,
            "expected_revision":1,"secret":B2}),
    )
    .expect_err("stale rotation must refuse");
    assert_eq!(error.code(), Some("stale_revision"));
    assert_eq!(
        must(rig.core_record(&id), "core read").to_json(),
        core_before,
        "stale rotation changed core authority"
    );
    assert_eq!(
        must(rig.vault_rows(&id), "vault row read").len(),
        rows_before,
        "stale rotation wrote a vault row"
    );
    must(rig.finish(), "rig finish");
}

/// (c) provider text hidden from public results; transmitted uncertainty kept.
#[test]
fn encrypted_test_send_hides_provider_text_and_keeps_uncertainty() {
    // 1) Accepted reply text carrying a non-secret canary.
    {
        let (_root, rig) = start();
        initialize(&rig);
        let mut peer = CanaryPeer::start(PeerMode::AcceptedCanary);
        let id = create(&rig, ACCOUNT, A, peer.port());
        bind(&rig, &id, "r2-canary-bind");
        let result = test_send(&rig);
        match &result {
            Ok(value) => assert!(
                !value.to_string().contains(CANARY),
                "public test-send status reflected provider-controlled text"
            ),
            Err(error) => assert!(
                !error.to_string().contains(CANARY),
                "public test-send error reflected provider-controlled text"
            ),
        }
        assert!(!format!("{result:?}").contains(A), "secret A reflected");
        assert_eq!(peer.sessions(), 1, "test-send retried automatically");
        must(peer.stop(), "r2 peer cleanup");
        must(rig.finish(), "rig finish");
    }

    // 2) A failure reply carrying provider text must not reach a public error.
    {
        let (_root, rig) = start();
        initialize(&rig);
        let mut peer = CanaryPeer::start(PeerMode::DeferredCanary);
        let id = create(&rig, ACCOUNT, A, peer.port());
        bind(&rig, &id, "r2-deferred-bind");
        let error = test_send(&rig).expect_err("a refused submission must be a safe error");
        assert!(
            !error.to_string().contains(CANARY),
            "public test-send error reflected provider-controlled text"
        );
        assert!(!error.to_string().contains(A), "secret A reflected");
        assert_eq!(
            peer.sessions(),
            1,
            "refused test-send retried automatically"
        );
        must(peer.stop(), "r2 peer cleanup");
        must(rig.finish(), "rig finish");
    }

    // 3) Peer reads the DATA terminator then closes: acceptance is unknowable.
    {
        let (_root, rig) = start();
        initialize(&rig);
        let mut peer = CanaryPeer::start(PeerMode::DropAfterData);
        let id = create(&rig, ACCOUNT, A, peer.port());
        bind(&rig, &id, "r2-uncertain-bind");
        let error = test_send(&rig).expect_err("a transmitted send with no reply must be unknown");
        assert_eq!(
            error.kind(),
            "unknown",
            "transmitted uncertainty was not preserved"
        );
        assert!(!error.to_string().contains(A), "secret A reflected");
        assert_eq!(
            peer.sessions(),
            1,
            "uncertain send was retried automatically"
        );
        must(peer.stop(), "r2 peer cleanup");
        must(rig.finish(), "rig finish");
    }
}

#[derive(Clone, Copy)]
enum PeerMode {
    /// Accept the message with a 250 whose text carries the canary.
    AcceptedCanary,
    /// Refuse at DATA with a 451 whose text carries the canary.
    DeferredCanary,
    /// Read the DATA terminator, then close without a reply.
    DropAfterData,
}

type PeerWire = BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;

fn wire_reply(wire: &mut PeerWire, text: &str) -> std::io::Result<()> {
    wire.get_mut().write_all(text.as_bytes())?;
    wire.get_mut().write_all(b"\r\n")?;
    wire.get_mut().flush()
}

fn wire_line(wire: &mut PeerWire) -> std::io::Result<Option<String>> {
    let mut text = String::new();
    let read = wire.take(65_536).read_line(&mut text)?;
    if read == 0 {
        return Ok(None);
    }
    if read == 65_536 || !text.ends_with('\n') {
        return Err(std::io::Error::other("peer line unavailable"));
    }
    Ok(Some(text.trim_end_matches(['\r', '\n']).to_string()))
}

/// One accepted peer session: the caller socket (for shutdown wakeups) and
/// its worker. Owned until joined on teardown.
struct PeerWorker {
    socket: TcpStream,
    thread: JoinHandle<()>,
}

fn join_peer_worker(handle: JoinHandle<()>, deadline: Instant) -> Result<()> {
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    if !handle.is_finished() {
        return Err(Error::internal("r2 peer cleanup deadline exceeded"));
    }
    handle
        .join()
        .map_err(|_| Error::internal("r2 peer worker panicked"))
}

/// One real TLS SMTP peer (fixture CA/cert). Every accepted session worker is
/// retained and joined on teardown; no session outlives the fixture root.
struct CanaryPeer {
    port: u16,
    sessions: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<PeerWorker>>>,
}

impl CanaryPeer {
    fn start(mode: PeerMode) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("peer bind");
        listener.set_nonblocking(true).expect("peer nonblocking");
        let port = listener.local_addr().expect("peer address").port();
        let tls = super::custody_fixture_tls_runtime::server_config().expect("fixture TLS peer");
        let sessions = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let workers = Arc::new(Mutex::new(Vec::new()));
        let accept = std::thread::spawn({
            let sessions = sessions.clone();
            let stop = stop.clone();
            let workers = workers.clone();
            move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            // A clone failure is a real setup/cleanup fault:
                            // surface it through the joined accept thread.
                            let socket = stream.try_clone().expect("r2 peer socket clone");
                            let tls = tls.clone();
                            let sessions = sessions.clone();
                            let thread = std::thread::spawn(move || {
                                sessions.fetch_add(1, Ordering::SeqCst);
                                let _ = serve_peer(stream, tls, mode);
                            });
                            workers
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .push(PeerWorker { socket, thread });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        Self {
            port,
            sessions,
            stop,
            accept: Some(accept),
            workers,
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn sessions(&self) -> u64 {
        self.sessions.load(Ordering::SeqCst)
    }

    /// Stop accepting, wake every blocked session socket, then join every
    /// owned worker within one bound. Failures are returned, never hidden.
    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + PEER_CLEANUP;
        let mut outcome = Ok(());
        if let Some(handle) = self.accept.take() {
            if let Err(error) = join_peer_worker(handle, deadline) {
                outcome = Err(error);
            }
        }
        let workers = {
            let mut guard = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *guard)
        };
        for worker in &workers {
            let _ = worker.socket.shutdown(Shutdown::Both);
        }
        for worker in workers {
            if let Err(error) = join_peer_worker(worker.thread, deadline) {
                outcome = Err(error);
            }
        }
        outcome
    }
}

impl Drop for CanaryPeer {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Drop cannot return an error; keep a fixed diagnostic like the
            // parent fixture owner. Successful teardown uses `stop`.
            eprintln!("CAD-1128 r2 peer cleanup did not complete successfully");
        }
    }
}

fn serve_peer(
    stream: TcpStream,
    tls: Arc<rustls::ServerConfig>,
    mode: PeerMode,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_nodelay(true)?;
    let conn = rustls::ServerConnection::new(tls).map_err(std::io::Error::other)?;
    let mut wire = BufReader::new(rustls::StreamOwned::new(conn, stream));
    wire_reply(&mut wire, "220 fixture.cadence ESMTP")?;
    loop {
        let Some(command) = wire_line(&mut wire)? else {
            return Ok(());
        };
        let upper = command.to_ascii_uppercase();
        if upper.starts_with("EHLO ") {
            wire_reply(&mut wire, "250-fixture.cadence")?;
            wire_reply(&mut wire, "250 AUTH LOGIN PLAIN")?;
        } else if upper == "AUTH LOGIN" {
            wire_reply(&mut wire, "334 VXNlcm5hbWU6")?;
            let _ = wire_line(&mut wire)?;
            wire_reply(&mut wire, "334 UGFzc3dvcmQ6")?;
            let _ = wire_line(&mut wire)?;
            wire_reply(&mut wire, "235 2.7.0 Accepted")?;
        } else if upper.starts_with("AUTH PLAIN") {
            if command.len() == 10 {
                wire_reply(&mut wire, "334 ")?;
                let _ = wire_line(&mut wire)?;
            }
            wire_reply(&mut wire, "235 2.7.0 Accepted")?;
        } else if upper.starts_with("MAIL FROM:") {
            wire_reply(&mut wire, "250 2.1.0 Ok")?;
        } else if upper.starts_with("RCPT TO:") {
            wire_reply(&mut wire, "250 2.1.5 Ok")?;
        } else if upper == "DATA" {
            wire_reply(&mut wire, "354 End data with <CR><LF>.<CR><LF>")?;
            loop {
                match wire_line(&mut wire)? {
                    Some(line) if line == "." => break,
                    Some(_) => {}
                    None => return Ok(()),
                }
            }
            match mode {
                PeerMode::AcceptedCanary => wire_reply(&mut wire, &format!("250 2.0.0 {CANARY}"))?,
                PeerMode::DeferredCanary => wire_reply(&mut wire, &format!("451 4.3.0 {CANARY}"))?,
                PeerMode::DropAfterData => return Ok(()),
            }
        } else if upper == "QUIT" {
            wire_reply(&mut wire, "221 Bye")?;
            return Ok(());
        } else {
            wire_reply(&mut wire, "500 5.5.1 Unrecognized")?;
        }
    }
}
