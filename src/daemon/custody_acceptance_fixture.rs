//! Genuine, test-only CAD-1128 rig: actual daemon/Store/sole vault manager,
//! verified TLS SMTP submission and cumulative wire observations. The frozen
//! driver owns assertions; this adapter owns only isolated setup and teardown.
#![cfg(test)]

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::daemon::{self, ServeOptions};
use crate::error::{Error, Result};
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

const BARRIER_TIMEOUT: Duration = Duration::from_secs(15);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmtpCounts {
    pub tcp_sessions: u64,
    pub accepted_messages: u64,
    pub auth_a: u64,
    pub auth_b: u64,
    pub auth_other: u64,
}

#[derive(Default)]
struct LiveCounts {
    tcp_sessions: AtomicU64,
    accepted_messages: AtomicU64,
    auth_a: AtomicU64,
    auth_b: AtomicU64,
    auth_other: AtomicU64,
}
impl LiveCounts {
    // Barrier snapshots follow joined workers. The S1 test-only peer hook
    // observes these atomics after its own DATA and before its acceptance.
    fn snapshot(&self) -> SmtpCounts {
        SmtpCounts {
            tcp_sessions: self.tcp_sessions.load(Ordering::SeqCst),
            accepted_messages: self.accepted_messages.load(Ordering::SeqCst),
            auth_a: self.auth_a.load(Ordering::SeqCst),
            auth_b: self.auth_b.load(Ordering::SeqCst),
            auth_other: self.auth_other.load(Ordering::SeqCst),
        }
    }
}

type TlsWire = BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;
fn reply(wire: &mut TlsWire, line: &str) -> std::io::Result<()> {
    wire.get_mut().write_all(line.as_bytes())?;
    wire.get_mut().write_all(b"\r\n")?;
    wire.get_mut().flush()
}
fn line(wire: &mut TlsWire) -> std::io::Result<String> {
    use std::io::Read;
    let mut text = String::new();
    let n = wire.take(65_536).read_line(&mut text)?;
    if n == 0 || n == 65_536 || !text.ends_with('\n') {
        return Err(std::io::Error::other("fixture SMTP line unavailable"));
    }
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}
fn b64(input: &str) -> Option<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.decode(input).ok()
}
// One actual ephemeral CA/server pair for this test process. Sealed child
// fixtures retain CA_PEM.to_vec() and smtp_config() without byte changes.
mod custody_fixture_tls_runtime;
static CA_PEM: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    custody_fixture_tls_runtime::ca_pem()
        .unwrap_or_else(|_| panic!("ephemeral fixture TLS root setup failed"))
});
fn smtp_config() -> Result<Arc<rustls::ServerConfig>> {
    custody_fixture_tls_runtime::server_config()
}

fn smtp_fixture_source(root: &Path) -> Result<PathBuf> {
    use sha2::{Digest, Sha256};

    let source = root.join("source");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apps/crm");
    for name in [
        "app.md",
        "app-chat.json",
        "app-assistant.json",
        "assets/crm.svg",
        "rubrics/email.md",
        "workflows/email-brief.md",
    ] {
        let to = source.join(name);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::copy(fixture.join(name), to)?;
    }
    // Match the current native SMTP declaration gate without modifying the
    // shared workflow fixture or weakening the production service boundary.
    let screen = source.join("screens/main");
    std::fs::create_dir_all(&screen)?;
    let js = "(()=>{})();";
    std::fs::write(screen.join("client.js"), js)?;
    let sha = |bytes: &[u8]| format!("sha256:{:x}", Sha256::digest(bytes));
    let declaration = json!({
        "contract": "app-screens/v2", "app": "crm", "entry": "client.js",
        "host_contract": "screen-records.v1", "may": ["records.slot"],
        "operations": ["smtp.bind"], "tools": {},
        "assets": [{"name": "client.js", "media_type": "text/javascript",
            "size": js.len(), "sha256": sha(js.as_bytes())}],
        "provenance": {"source_digest": sha(b"cad1128-source"),
            "sdk_digest": sha(b"cad1128-sdk"), "toolchain_digest": sha(b"cad1128-toolchain")}
    });
    std::fs::write(screen.join("screens.json"), declaration.to_string())?;
    Ok(source)
}

// Implicit TLS is the actual mode selected by the frozen driver's envelope.
// All TCP accepts (including failed TLS) are counted in the accept loop.
fn session(
    stream: TcpStream,
    tls: Arc<rustls::ServerConfig>,
    counts: Arc<LiveCounts>,
    a: Arc<Vec<u8>>,
    b: Arc<Vec<u8>>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_nodelay(true)?;
    let conn = rustls::ServerConnection::new(tls).map_err(std::io::Error::other)?;
    let mut wire = BufReader::new(rustls::StreamOwned::new(conn, stream));
    let deadline = Instant::now() + Duration::from_secs(10);
    reply(&mut wire, "220 fixture.cadence ESMTP")?;
    let mut authenticated = false;
    let mut mail = false;
    let mut recipient = false;
    while Instant::now() < deadline {
        let command = line(&mut wire)?;
        let upper = command.to_ascii_uppercase();
        if upper.starts_with("EHLO ") {
            reply(&mut wire, "250-fixture.cadence")?;
            reply(&mut wire, "250 AUTH PLAIN LOGIN")?;
        } else if upper.starts_with("AUTH ") {
            authenticated = false;
            let credential = if upper.starts_with("AUTH PLAIN") {
                let initial = command.get(10..).unwrap_or("").trim();
                let payload = if initial.is_empty() {
                    reply(&mut wire, "334 ")?;
                    line(&mut wire)?
                } else {
                    initial.to_string()
                };
                b64(&payload).and_then(|decoded| {
                    let fields: Vec<_> = decoded.split(|b| *b == 0).collect();
                    if fields.len() != 3 || !fields[0].is_empty() {
                        return None;
                    }
                    Some((fields[1].to_vec(), fields[2].to_vec()))
                })
            } else if upper == "AUTH LOGIN" {
                reply(&mut wire, "334 VXNlcm5hbWU6")?;
                let username = b64(&line(&mut wire)?);
                reply(&mut wire, "334 UGFzc3dvcmQ6")?;
                let password = b64(&line(&mut wire)?);
                username.zip(password)
            } else {
                None
            };
            match credential {
                Some((user, password))
                    if user == b"fixture-user" && password.as_slice() == a.as_slice() =>
                {
                    counts.auth_a.fetch_add(1, Ordering::SeqCst);
                    authenticated = true;
                }
                Some((user, password))
                    if user == b"fixture-user" && password.as_slice() == b.as_slice() =>
                {
                    counts.auth_b.fetch_add(1, Ordering::SeqCst);
                    authenticated = true;
                }
                _ => {
                    counts.auth_other.fetch_add(1, Ordering::SeqCst);
                }
            }
            reply(
                &mut wire,
                if authenticated {
                    "235 authenticated"
                } else {
                    "535 authentication refused"
                },
            )?;
        } else if upper.starts_with("MAIL FROM:") && authenticated {
            mail = true;
            recipient = false;
            reply(&mut wire, "250 sender accepted")?;
        } else if upper.starts_with("RCPT TO:") && authenticated && mail {
            recipient = true;
            reply(&mut wire, "250 recipient accepted")?;
        } else if upper == "DATA" && authenticated && mail && recipient {
            reply(&mut wire, "354 end with .")?;
            let mut bytes = 0usize;
            loop {
                if Instant::now() >= deadline || bytes > 2_000_000 {
                    return Err(std::io::Error::other("fixture SMTP DATA deadline/bound"));
                }
                let data = line(&mut wire)?;
                if data == "." {
                    break;
                }
                bytes += data.len();
            }
            #[cfg(all(test, target_os = "linux", feature = "test-seam"))]
            smtp_scope7::provider_at(wire.get_ref().sock.local_addr()?.port(), counts.snapshot())
                .map_err(|_| std::io::Error::other("S1 owned provider gate failed"))?;
            // CAD-1128 unit 4: hold the terminal `250` past the client's
            // real IO_TIMEOUT so a transmitted send lands in `Uncertain`.
            // Disarmed = inert no-op; armed = parks until released (≤40 s).
            #[cfg(all(test, target_os = "linux", feature = "test-seam"))]
            smtp_timeout4::hold_reply(
                wire.get_ref().sock.local_addr()?.port(),
                std::time::Duration::from_secs(40),
            )
            .map_err(|_| std::io::Error::other("RT4 owned reply-hold failed"))?;
            // Counter represents a complete DATA accepted by the genuine peer.
            counts.accepted_messages.fetch_add(1, Ordering::SeqCst);
            reply(&mut wire, "250 queued")?;
            mail = false;
            recipient = false;
        } else if upper == "RSET" {
            mail = false;
            recipient = false;
            reply(&mut wire, "250 reset")?;
        } else if upper == "NOOP" {
            reply(&mut wire, "250 ok")?;
        } else if upper == "QUIT" {
            reply(&mut wire, "221 bye")?;
            return Ok(());
        } else {
            reply(&mut wire, "530 authenticate and supply envelope first")?;
        }
    }
    Err(std::io::Error::other("fixture SMTP session deadline"))
}

struct Worker {
    socket: TcpStream,
    thread: JoinHandle<()>,
}
fn join_before<T>(handle: JoinHandle<T>, deadline: Instant) -> Result<T> {
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    if !handle.is_finished() {
        return Err(Error::internal("fixture worker cleanup deadline exceeded"));
    }
    handle
        .join()
        .map_err(|_| Error::internal("fixture worker panicked"))
}
fn drain_accepts(
    listener: &TcpListener,
    workers: &mut Vec<Worker>,
    tls: &Arc<rustls::ServerConfig>,
    counts: &Arc<LiveCounts>,
    a: &Arc<Vec<u8>>,
    b: &Arc<Vec<u8>>,
    deadline: Instant,
) -> Result<()> {
    loop {
        if Instant::now() >= deadline {
            return Err(Error::internal("fixture accept drain deadline"));
        }
        match listener.accept() {
            Ok((stream, _)) => {
                counts.tcp_sessions.fetch_add(1, Ordering::SeqCst);
                let socket = stream.try_clone()?;
                let tls = tls.clone();
                let counts = counts.clone();
                let a = a.clone();
                let b = b.clone();
                let thread = thread::spawn(move || {
                    let _ = session(stream, tls, counts, a, b);
                });
                workers.push(Worker { socket, thread });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(_) => return Err(Error::internal("fixture SMTP accept failed")),
        }
    }
}
// Control messages are consumed by the listener owner, not an independent
// sleep/snapshot reader. The acknowledgement follows accept-until-WouldBlock
// and joining every worker admitted before that boundary.
fn smtp_loop(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    counts: Arc<LiveCounts>,
    a: Arc<Vec<u8>>,
    b: Arc<Vec<u8>>,
    stop: Arc<AtomicBool>,
    barriers: mpsc::Receiver<mpsc::SyncSender<Result<SmtpCounts>>>,
) -> Result<()> {
    let mut workers: Vec<Worker> = Vec::new();
    let run = (|| -> Result<()> {
        while !stop.load(Ordering::SeqCst) {
            drain_accepts(
                &listener,
                &mut workers,
                &tls,
                &counts,
                &a,
                &b,
                Instant::now() + BARRIER_TIMEOUT,
            )?;
            match barriers.recv_timeout(Duration::from_millis(5)) {
                Ok(ack) => {
                    let deadline = Instant::now() + BARRIER_TIMEOUT;
                    let result = (|| {
                        drain_accepts(&listener, &mut workers, &tls, &counts, &a, &b, deadline)?;
                        // Do not take workers out of their RAII owner until they
                        // finish; on timeout the cleanup below closes sockets.
                        while workers.iter().any(|w| !w.thread.is_finished()) {
                            if Instant::now() >= deadline || stop.load(Ordering::SeqCst) {
                                return Err(Error::internal("fixture SMTP barrier deadline"));
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        let mut joined = Ok(());
                        for worker in workers.drain(..) {
                            if let Err(error) = join_before(worker.thread, deadline) {
                                joined = Err(error);
                            }
                        }
                        joined?;
                        Ok(counts.snapshot())
                    })();
                    let failed = result.is_err();
                    let _ = ack.send(result);
                    if failed {
                        return Err(Error::internal("fixture SMTP barrier failed"));
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        Ok(())
    })();
    // Only sockets owned by this rig. Shutdown wakes blocked TLS reads/writes.
    for worker in &workers {
        let _ = worker.socket.shutdown(Shutdown::Both);
    }
    drop(listener);
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    let mut cleanup = Ok(());
    for worker in workers {
        if let Err(e) = join_before(worker.thread, deadline) {
            cleanup = Err(e);
        }
    }
    run.and(cleanup)
}

// Exists before either thread starts, so every setup ? also requests and
// observes shutdown. It does not pretend to own the caller's TempDir.
struct Owner {
    smtp_stop: Arc<AtomicBool>,
    daemon_stop: Arc<AtomicBool>,
    smtp_thread: Option<JoinHandle<Result<()>>>,
    daemon_thread: Option<JoinHandle<Result<()>>>,
}
impl Owner {
    fn new() -> Self {
        Self {
            smtp_stop: Arc::new(AtomicBool::new(false)),
            daemon_stop: Arc::new(AtomicBool::new(false)),
            smtp_thread: None,
            daemon_thread: None,
        }
    }
    fn stop(&mut self) -> Result<()> {
        self.daemon_stop.store(true, Ordering::SeqCst);
        self.smtp_stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + CLEANUP_TIMEOUT + CLEANUP_TIMEOUT;
        let mut outcome = Ok(());
        // Join both even if the first reports failure; no unconditional Ok.
        for handle in [self.smtp_thread.take(), self.daemon_thread.take()]
            .into_iter()
            .flatten()
        {
            if let Err(error) = join_before(handle, deadline).and_then(|r| r) {
                outcome = Err(error);
            }
        }
        outcome
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.stop().is_err() {
            // Drop cannot return an error; preserve a fixed diagnostic for
            // setup-error/unwind cleanup. Successful completion uses finish.
            eprintln!("CAD-1128 fixture cleanup did not complete successfully");
        }
    }
}

pub struct Rig {
    pub state_dir: PathBuf,
    pub core_db_path: PathBuf,
    pub workspace_id: String,
    pub smtp_port: u16,
    pub install_id: String,
    pub context_id: String,
    pub campaign_id: String,
    owner: Mutex<Owner>,
    barriers: mpsc::Sender<mpsc::SyncSender<Result<SmtpCounts>>>,
    shared: Arc<super::Shared>,
}
fn wait_socket(state_dir: &Path, owner: &Owner, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if owner
            .daemon_thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
        {
            return Err(Error::internal("fixture daemon exited before readiness"));
        }
        if crate::client::rpc_private_timeout(
            state_dir,
            "health",
            json!({}),
            Duration::from_secs(1),
        )
        .is_ok()
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(Error::internal("fixture daemon readiness deadline"))
}
impl Rig {
    /// Encrypted-custody rig (CAD-1128 under-test backend).
    pub fn start(root: &Path, a: &[u8], b: &[u8]) -> Result<Self> {
        Self::start_with_custody(root, a, b, true)
    }

    /// Legacy-custody rig: identical real daemon/TLS/RPC, but the custody
    /// backend uses the ordinary legacy selector (no encrypted vault). This is the
    /// positive-control daemon for the unsupported-consumer canaries — a
    /// FILE/SMTP enrollment it accepts is refused on the encrypted rig, so
    /// the control must run here. Test-only.
    pub fn start_legacy(root: &Path, a: &[u8], b: &[u8]) -> Result<Self> {
        Self::start_with_custody(root, a, b, false)
    }

    /// Adapter injection is confined to this isolated test fixture. The
    /// supplied metadata prevents attach from loading host deployment files.
    pub fn start_with_platforms(
        root: &Path,
        a: &[u8],
        b: &[u8],
        encrypted: bool,
        extra_platforms: super::effect_rpc::PlatformMap,
        provider_deployments: Option<crate::platform::deployments::DeploymentMetadata>,
    ) -> Result<Self> {
        let deployments = provider_deployments.ok_or_else(|| {
            Error::internal("fixture adapter injection requires isolated deployment metadata")
        })?;
        Self::start_with_custody_ex(
            root,
            a,
            b,
            encrypted,
            Some(extra_platforms),
            Some(deployments),
            None,
        )
    }

    /// Stop this owned daemon/peer, retain its real state and key, then start
    /// a fresh encrypted session without replacing existing configuration.
    pub fn restart_encrypted_with_platforms(
        self,
        a: &[u8],
        b: &[u8],
        extra_platforms: super::effect_rpc::PlatformMap,
        provider_deployments: Option<crate::platform::deployments::DeploymentMetadata>,
    ) -> Result<Self> {
        let deployments = provider_deployments.ok_or_else(|| {
            Error::internal("fixture adapter injection requires isolated deployment metadata")
        })?;
        let Self {
            state_dir,
            install_id,
            context_id,
            campaign_id,
            owner,
            barriers,
            shared,
            ..
        } = self;
        let mut owner = owner
            .into_inner()
            .map_err(|_| Error::internal("fixture owner poisoned"))?;
        owner.stop()?;
        drop(barriers);
        drop(shared);
        let root = state_dir
            .parent()
            .ok_or_else(|| Error::internal("fixture root unavailable"))?;
        Self::start_with_custody_ex(
            root,
            a,
            b,
            true,
            Some(extra_platforms),
            Some(deployments),
            Some((install_id, context_id, campaign_id)),
        )
    }

    fn start_with_custody(root: &Path, a: &[u8], b: &[u8], encrypted: bool) -> Result<Self> {
        Self::start_with_custody_ex(root, a, b, encrypted, None, None, None)
    }

    fn start_with_custody_ex(
        root: &Path,
        a: &[u8],
        b: &[u8],
        encrypted: bool,
        extra_platforms: Option<super::effect_rpc::PlatformMap>,
        provider_deployments: Option<crate::platform::deployments::DeploymentMetadata>,
        retained: Option<(String, String, String)>,
    ) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        eprintln!("CAD-1128 fixture linked SQLite {}", rusqlite::version());
        if encrypted {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::symlink_metadata(root)?;
            // Observe the real strict path guard without changing admission,
            // permissions, caller identity, or any key/vault bytes. This pins
            // the first setup refusal instead of guessing from storage errors.
            let admission = crate::platform::ControlledDirectories::open(root, false);
            eprintln!(
                "CAD-1128 fixture root admission: mode={:04o} owner={} accepted={}",
                metadata.mode() & 0o7777,
                metadata.uid(),
                admission.is_ok()
            );
        }
        let state_dir = root.join("state");
        let pm_dir = root.join("pm");
        let key_dir = root.join("keys");
        let key_path = key_dir.join("key");
        if retained.is_some() {
            for dir in [&state_dir, &pm_dir, &key_dir] {
                crate::platform::ControlledDirectories::open(dir, false)?;
            }
            crate::issue::Pm::at(&pm_dir)?;
            if !std::fs::symlink_metadata(&key_path)?.is_file() {
                return Err(Error::internal("fixture existing key unavailable"));
            }
        } else {
            std::fs::DirBuilder::new().mode(0o700).create(&state_dir)?;
            // Host umask is not a privacy guarantee. Reserve the fixture's PM
            // root ourselves before the real initializer populates it.
            std::fs::DirBuilder::new().mode(0o700).create(&pm_dir)?;
            crate::issue::Pm::init(&pm_dir)?;
            std::fs::DirBuilder::new().mode(0o700).create(&key_dir)?;
            let mut key = zeroize::Zeroizing::new([0u8; 32]);
            getrandom::fill(key.as_mut())
                .map_err(|_| Error::internal("fixture key entropy unavailable"))?;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&key_path)?;
            file.write_all(key.as_slice())?;
            file.sync_all()?;
            drop(file);
            std::fs::File::open(&key_dir)?.sync_all()?;
        }
        // The daemon and real standalone artifacts see the same owner record.
        // Paths only: the synthetic key bytes never enter this JSON.
        // Only the encrypted rig writes the custody config record — a legacy
        // rig must leave it absent so the daemon selects the legacy backend
        // instead of auto-adopting encrypted custody from the record.
        if encrypted {
            let config_path = state_dir.join(crate::platform::encrypted_config::RECORD);
            if !(retained.is_some() && config_path.try_exists()?) {
                let mut config = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&config_path)?;
                config.write_all(&serde_json::to_vec(&json!({
                    "key_file": key_path, "vault_dir": state_dir.join("vault")
                }))?)?;
                config.sync_all()?;
                drop(config);
                std::fs::File::open(&state_dir)?.sync_all()?;
            }
        }

        let mut owner = Owner::new();
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let smtp_port = listener.local_addr()?.port();
        let tls = smtp_config()?;
        let (barriers, receiver) = mpsc::channel();
        let counts = Arc::new(LiveCounts::default());
        owner.smtp_thread = Some(thread::spawn({
            let stop = owner.smtp_stop.clone();
            let a = Arc::new(a.to_vec());
            let b = Arc::new(b.to_vec());
            move || smtp_loop(listener, tls, counts, a, b, stop, receiver)
        }));
        // Binding is synchronous. No counted asynchronous readiness connection.
        let observer = Arc::new(Mutex::new(None));
        let mut opts = ServeOptions {
            custody_acceptance_shared: Some(observer.clone()),
            stop: Some(owner.daemon_stop.clone()),
            test_seam: true,
            smtp_test_ca_pem: Some(CA_PEM.to_vec()),
            ..ServeOptions::default()
        };
        opts.provider_env.set(
            "CADENCE_PM_DIR",
            pm_dir
                .to_str()
                .ok_or_else(|| Error::internal("fixture path encoding"))?,
        );
        if encrypted {
            opts.encrypted_custody =
                Some(crate::platform::encrypted_sqlite::EncryptedCustodyConfig {
                    key_file: key_path,
                    vault_dir: state_dir.join("vault"),
                });
        }
        opts.auto_stop = Some(daemon::AutoStopSetting::off());
        opts.report_router = Some(0);
        crate::platform::local::register(&state_dir, &mut opts);
        crate::platform::smtp::attach(&mut opts);
        if let Some(deployments) = provider_deployments {
            opts.provider_deployments = Some(deployments);
        }
        if let Some(platforms) = extra_platforms {
            opts.platforms.extend(platforms);
        }
        owner.daemon_thread = Some(thread::spawn({
            let state = state_dir.clone();
            move || {
                let served = daemon::serve_with(&state, opts);
                if let Err(error) = &served {
                    // Fixed stage/type/code only, never raw paths/input or an
                    // error payload that might contain credential material.
                    eprintln!(
                        "CAD-1128 fixture daemon exit: kind={} code={}",
                        error.kind(),
                        error.code().unwrap_or("none")
                    );
                    if error.to_string().contains("not under the test temp") {
                        eprintln!("CAD-1128 fixture root is outside runner TMPDIR confinement");
                    }
                }
                served
            }
        }));
        wait_socket(&state_dir, &owner, Duration::from_secs(30))?;
        let shared = observer
            .lock()
            .map_err(|_| Error::internal("fixture observer poisoned"))?
            .clone()
            .ok_or_else(|| Error::internal("real daemon observer absent"))?;
        let rpc = |method: &str, params: Value| -> Result<Value> {
            scoped(Asserted::Operator, || {
                crate::client::rpc_private_timeout(
                    &state_dir,
                    method,
                    params,
                    Duration::from_secs(30),
                )
            })
        };
        let (install_id, context_id, campaign_id) = if let Some(ids) = retained {
            ids
        } else {
            let install = rpc(
                "app_workspace_install",
                json!({"source": smtp_fixture_source(root)?}),
            )?;
            let install_id = install["install_id"]
                .as_str()
                .ok_or_else(|| Error::internal("fixture install id absent"))?
                .to_string();
            let context = rpc(
                "app_context_create",
                json!({"install_id": install_id, "label": "Acct", "input_defaults": {}, "request_id": "rig-ctx"}),
            )?;
            let context_id = context["context"]["id"]
                .as_str()
                .ok_or_else(|| Error::internal("fixture context id absent"))?
                .to_string();
            let campaign_id = "rig-campaign".to_string();
            // Exactly the production allowlist; context request_id is not copied
            // into content-save, whose protocol intentionally has no such field.
            rpc(
                "app_content_save",
                json!({"install_id": install_id, "context_id": context_id,
                "campaign_id": campaign_id, "subject": "Hi", "preheader": "Pre",
                "blocks": [{"type": "paragraph", "text": "Body."}]}),
            )?;
            (install_id, context_id, campaign_id)
        };
        let workspace_id = shared.store.connection_workspace_id()?;
        Ok(Self {
            core_db_path: state_dir.join("cadence.sqlite3"),
            state_dir,
            workspace_id,
            smtp_port,
            install_id,
            context_id,
            campaign_id,
            owner: Mutex::new(owner),
            barriers,
            shared,
        })
    }
    pub fn smtp_barrier(&self) -> Result<SmtpCounts> {
        let (ack, observed) = mpsc::sync_channel(1);
        self.barriers
            .send(ack)
            .map_err(|_| Error::internal("fixture SMTP listener stopped"))?;
        let counts = observed
            .recv_timeout(BARRIER_TIMEOUT + Duration::from_secs(1))
            .map_err(|_| Error::internal("fixture SMTP acknowledgement deadline"))??;
        eprintln!("CAD-1128 fixture cumulative SMTP barrier: {counts:?}");
        Ok(counts)
    }
    pub fn core_record(&self, id: &str) -> Result<crate::store::CredentialRecord> {
        self.shared
            .store
            .connection_credential(id)?
            .ok_or_else(|| Error::internal("fixture core authority absent"))
    }
    pub fn vault_rows(
        &self,
        id: &str,
    ) -> Result<Vec<crate::platform::encrypted_sqlite::RowMetadata>> {
        let _operation = self.shared.custody_operation()?;
        self.shared
            .platform_custody
            .encrypted()
            .ok_or_else(|| Error::internal("fixture vault absent"))?
            .rows_for(id)
    }
    pub fn read_row_using_its_stored_identity(
        &self,
        row: &crate::platform::encrypted_sqlite::RowMetadata,
    ) -> Result<crate::platform::smtp::SmtpEnvelope> {
        let _operation = self.shared.custody_operation()?;
        let vault = self
            .shared
            .platform_custody
            .encrypted()
            .ok_or_else(|| Error::internal("fixture vault absent"))?;
        let expected = crate::platform::encrypted_sqlite::ExpectedIdentity {
            workspace_id: row.workspace_id.clone(),
            connection_id: row.connection_id.clone(),
            provider: row.provider.clone(),
            account: row.account.clone(),
            exchange: row.exchange.clone(),
            credential_revision: row.credential_revision,
            key_id: vault
                .key_id()
                .ok_or_else(|| Error::invalid("locked", "fixture custody locked"))?,
        };
        let bytes = vault.read_exact(&row.reference, &expected)?;
        Ok(crate::platform::smtp::custody_decode(&bytes)?.0)
    }
    /// The live daemon `Shared` — test-only, for admission canaries that
    /// must reach `pub(super)`/`self: &Arc<Self>` internal entry points
    /// (e.g. `crm_send_run`, `spawn_crm_send_worker`) on a send seeded
    /// directly as a stale prepared row. Never exposes custody bytes.
    #[cfg(test)]
    pub fn shared(&self) -> Arc<super::Shared> {
        self.shared.clone()
    }
    pub fn finish(self) -> Result<()> {
        self.owner
            .lock()
            .map_err(|_| Error::internal("fixture owner poisoned"))?
            .stop()
    }
}

mod artifact_acceptance;
mod authority5;
#[cfg(all(test, target_os = "linux", feature = "test-seam"))]
mod custody6;
#[cfg(all(test, target_os = "linux", feature = "test-seam"))]
mod r2_gap_regressions;

#[cfg(all(test, target_os = "linux", feature = "test-seam"))]
pub(super) mod smtp_scope7;
#[cfg(all(test, target_os = "linux", feature = "test-seam"))]
pub(super) mod smtp_timeout4;
