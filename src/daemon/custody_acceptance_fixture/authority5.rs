// Independent owner: cad1085-security. Original r2 contract SHA-256:
// 3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8
// Mount ONLY as a child of daemon::custody_acceptance_fixture, unchanged.
// The parent fixture supplies real daemon/TLS observations; no product API.
#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("authority5 requires an admitted Linux test-seam unit-test runner; never skip");

use super::Rig;
use crate::daemon::caller_rule::Who;
use crate::operator_auth::Origin;
use crate::store::NewAgent;
use rusqlite::{params, types::ValueRef, Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const A: &str = "YAwS5AyAesmZzdAuOBbBykufIyVNQQunwIqfTTWJlu4";
const B: &str = "Lc6BitPRSkgpooTCxJPhIuQaLtngFLkWw5MpFl5l4HY";
const TARGET: &str = "authority5-denied-target";
const PYTHON_HASH: &str = "9f6620b68b09e166ff3c4e50c73ab4083bcb727129e8aa660bf404e401a3ef97";
const PYTHON_PROBE: &str = include_str!("native_probe.py");
const WAIT: Duration = Duration::from_secs(15);

fn must<T, E>(r: std::result::Result<T, E>, stage: &'static str) -> T {
    r.unwrap_or_else(|_| panic!("{stage}"))
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn no_secret(value: &Value) {
    let bytes = value.to_string();
    assert!(
        !bytes.contains(A) && !bytes.contains(B),
        "synthetic credential leaked in client result"
    );
}

// SETUP/ORACLE ONLY: explicit scoped simulation to create usable preconditions.
// Native requests below never call this function or a Cadence client helper.
fn setup_rpc(rig: &Rig, method: &str, params: Value) -> Value {
    let v = must(
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc_private_timeout(&rig.state_dir, method, params, WAIT)
        }),
        "synthetic authority setup/control failed",
    );
    no_secret(&v);
    v
}

struct Probe {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
    pid: u32,
    sid: u32,
}
impl Probe {
    fn start(detached: bool) -> Self {
        assert_eq!(
            digest(PYTHON_PROBE.as_bytes()),
            PYTHON_HASH,
            "independent Python probe bytes changed"
        );
        // No env_clear/remove/set, identity substitution, installed binary or shell.
        let mut child = must(
            Command::new("python3")
                .arg("-c")
                .arg(PYTHON_PROBE)
                .arg(if detached { "setsid" } else { "attached" })
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn(),
            "real native probe spawn failed",
        );
        let input = child.stdin.take();
        let stdout = child.stdout.take().expect("probe stdout absent");
        let (tx, output) = mpsc::channel::<Value>();
        let reader = thread::spawn(move || {
            let mut source = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let n = match source.by_ref().take(512 * 1024 + 1).read_line(&mut line) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 || n > 512 * 1024 || !line.ends_with('\n') {
                    break;
                }
                let Ok(v) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(v).is_err() {
                    break;
                }
            }
        });
        let ready = must(output.recv_timeout(WAIT), "native probe readiness failed");
        assert_eq!(ready["ready"], true, "native probe setup failed");
        let pid = ready["pid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .expect("probe pid absent");
        let sid = ready["sid"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .expect("probe session absent");
        assert_eq!(pid, child.id(), "ready pid differs from owned Child");
        if detached {
            assert_eq!(pid, sid, "setsid did not create a real new session");
        }
        Self {
            child,
            input,
            output,
            reader: Some(reader),
            pid,
            sid,
        }
    }
    fn request(&mut self, value: Value) -> Value {
        must(
            writeln!(self.input.as_mut().expect("probe stdin absent"), "{value}"),
            "probe input failed",
        );
        let result = must(
            self.output.recv_timeout(WAIT),
            "native probe transport/deadline failure",
        );
        assert!(
            result.get("probe_error").is_none(),
            "native probe setup/transport error; not behavioral refusal"
        );
        no_secret(&result);
        result
    }
    fn rpc(&mut self, rig: &Rig, method: &str, params: Value) -> Value {
        self.request(
            json!({"transport":"rpc", "socket":rig.state_dir.join("cadence.sock"),
                           "method":method, "params":params}),
        )["rpc"]
            .clone()
    }
    fn finish(&mut self) {
        if self.input.is_none() {
            return;
        }
        assert_eq!(self.request(json!({"transport":"stop"}))["stopped"], true);
        self.input.take();
        let deadline = Instant::now() + WAIT;
        loop {
            match must(self.child.try_wait(), "probe wait failed") {
                Some(status) => {
                    assert!(status.success(), "native probe failed");
                    break;
                }
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                None => panic!("native probe cleanup deadline"),
            }
        }
        if let Some(handle) = self.reader.take() {
            must(handle.join(), "probe reader failed");
        }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        self.input.take();
        // Kill/wait ONLY our own child on error/unwind, never another process.
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }
}

struct Board {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<crate::Result<()>>>,
    port: u16,
}
impl Board {
    fn start(rig: &Rig, root: &Path) -> Self {
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("no authorized isolated board port available");
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(tx),
            test_seam: false,
            ..Default::default()
        };
        let (state, pm) = (rig.state_dir.clone(), root.join("pm"));
        let handle = thread::spawn(move || crate::ui::serve(&state, &pm, &opts));
        let board = Self {
            stop,
            thread: Some(handle),
            port,
        };
        must(
            must(rx.recv_timeout(WAIT), "real board startup disconnected"),
            "real board failed to bind",
        );
        board
    }
    fn session(&self, rig: &Rig) -> crate::operator_auth::Opened {
        // Actual random session persisted by daemon Auth. Fixture provisioning,
        // not ui login, operator approval, or native positive-authority evidence.
        let now = rig.shared.operator_now();
        let mut auth = rig.shared.operator_auth();
        let nonce = must(
            auth.mint(Origin::Loopback, now),
            "fixture session mint failed",
        );
        must(
            must(
                auth.open(&nonce, Origin::Loopback, "authority5", now),
                "fixture session rejected",
            ),
            "fixture session persistence failed",
        )
    }
    fn http(&self, rig: &Rig, probe: &mut Probe, method: &str, path: &str, body: Value) -> Value {
        let session = self.session(rig);
        // Positive credential control on this VERY native child before its HTTP
        // request. A missing/expired cookie or wrong key must not explain denial.
        let checked = probe.rpc(
            rig,
            "operator_session_check",
            json!({
            "token":session.token,"key":session.key,"origin":"loopback"}),
        );
        assert_eq!(
            checked["ok"], true,
            "actual session check failed before HTTP denial"
        );
        assert_eq!(
            checked["result"]["valid"], true,
            "HTTP credential was not valid before denial"
        );
        assert_eq!(
            checked["result"]["session"]["id"], session.session.id,
            "HTTP precondition did not have the fresh actual session"
        );
        probe.request(json!({"transport":"http", "port":self.port,
            "host":format!("cadence-{}.localhost:{}",self.port,self.port),
            "verb":method,"path":path,"body":body,"token":session.token,"key":session.key}))["http"]
            .clone()
    }
    fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let deadline = Instant::now() + WAIT;
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(handle.is_finished(), "owned board shutdown deadline");
            must(
                must(handle.join(), "board thread panicked"),
                "board serve/cleanup failed",
            );
        }
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            let deadline = Instant::now() + WAIT;
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if handle.is_finished() {
                let _ = handle.join();
            } else {
                eprintln!("authority5 owned board cleanup incomplete");
            }
        }
    }
}

// Native Linux fd observation of opens/accesses/mutations on synthetic key.
// No key read in observation, no monkey-patched return or inferred atime count.
struct KeyWatch(OwnedFd);
impl KeyWatch {
    fn start(key: &Path) -> Self {
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(raw >= 0, "native key watcher unavailable");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        use std::os::unix::ffi::OsStrExt;
        let path = must(
            CString::new(key.as_os_str().as_bytes()),
            "watch path invalid",
        );
        let result = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_OPEN
                    | libc::IN_ACCESS
                    | libc::IN_MODIFY
                    | libc::IN_ATTRIB
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF,
            )
        };
        assert!(result >= 0, "actual synthetic key watch failed");
        Self(fd)
    }
    fn drain(&self) -> usize {
        let mut total = 0;
        loop {
            let mut buf = [0u8; 8192];
            let n = unsafe { libc::read(self.0.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock,
                    "key watch observation failed"
                );
                return total;
            }
            assert!(n > 0, "key watch unexpectedly closed");
            // Queue overflow/ignored watch are failures, not absence of access.
            let mut offset = 0;
            while offset + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let event = unsafe {
                    std::ptr::read_unaligned(buf.as_ptr().add(offset).cast::<libc::inotify_event>())
                };
                assert_eq!(
                    event.mask & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED),
                    0,
                    "key watcher lost observation capacity"
                );
                offset += std::mem::size_of::<libc::inotify_event>() + event.len as usize;
                total += 1;
            }
            assert_eq!(offset, n as usize, "malformed native watch event");
        }
    }
    fn calibrate(&self, key: &Path) {
        assert_eq!(self.drain(), 0, "key observation dirty before calibration");
        let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
        must(
            must(
                std::fs::File::open(key),
                "synthetic raw key readability control failed",
            )
            .read_exact(bytes.as_mut()),
            "synthetic key read control failed",
        );
        assert!(
            self.drain() > 0,
            "key watch did not observe real calibration access"
        );
    }
}

#[derive(PartialEq, Eq)]
struct Protected {
    core: BTreeMap<String, Vec<String>>,
    vault: BTreeMap<PathBuf, (String, u32, u32, u64)>,
    status: String,
    smtp: super::SmtpCounts,
}
fn core_snapshot(rig: &Rig) -> BTreeMap<String, Vec<String>> {
    let conn = must(
        Connection::open_with_flags(
            &rig.core_db_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ),
        "core observation failed",
    );
    let names = must(
        conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"),
        "core table census failed",
    )
    .query_map([], |row| row.get::<_, String>(0))
    .expect("core table census refused")
    .collect::<rusqlite::Result<Vec<_>>>()
    .expect("core table census decode failed");
    let mut result = BTreeMap::new();
    for name in names.into_iter().filter(|n| {
        n.starts_with("platform_")
            || matches!(n.as_str(), "effects" | "drafts")
            || n.starts_with("app_effect")
            || n.starts_with("app_approval")
            || n.starts_with("app_binding")
            || matches!(n.as_str(), "app_grants" | "connection_metadata")
    }) {
        // Catalog identifiers are quoted, never interpolated without escaping.
        let sql = format!("SELECT * FROM \"{}\"", name.replace('"', "\"\""));
        let mut stmt = must(conn.prepare(&sql), "protected core scan failed");
        let columns = stmt.column_count();
        let mut rows = must(stmt.query([]), "protected core scan query failed");
        let mut observations = Vec::new();
        while let Some(row) = must(rows.next(), "protected core row unavailable") {
            let mut encoded = Vec::new();
            for index in 0..columns {
                let value = must(row.get_ref(index), "protected core column unavailable");
                let (tag, bytes): (u8, Vec<u8>) = match value {
                    ValueRef::Null => (0, vec![]),
                    ValueRef::Integer(v) => (1, v.to_le_bytes().to_vec()),
                    ValueRef::Real(v) => (2, v.to_bits().to_le_bytes().to_vec()),
                    ValueRef::Text(v) => (3, v.to_vec()),
                    ValueRef::Blob(v) => (4, v.to_vec()),
                };
                encoded.push(tag);
                encoded.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                encoded.extend_from_slice(&bytes);
            }
            observations.push(digest(&encoded));
        }
        observations.sort();
        result.insert(name, observations);
    }
    assert!(
        result.contains_key("platform_credentials"),
        "protected core census missing credentials"
    );
    result
}
fn vault_snapshot(root: &Path, at: &Path, output: &mut BTreeMap<PathBuf, (String, u32, u32, u64)>) {
    if !at.exists() {
        return;
    }
    let meta = must(at.symlink_metadata(), "vault namespace unavailable");
    assert!(
        !meta.file_type().is_symlink(),
        "unexpected fixture vault alias"
    );
    if meta.is_dir() {
        for entry in must(std::fs::read_dir(at), "vault directory observation failed") {
            vault_snapshot(root, &must(entry, "vault entry unavailable").path(), output);
        }
    } else {
        assert!(meta.is_file(), "unexpected vault file class");
        output.insert(
            must(at.strip_prefix(root), "vault path escaped root").to_path_buf(),
            (
                digest(&must(std::fs::read(at), "vault bytes unavailable")),
                meta.mode(),
                meta.uid(),
                meta.nlink(),
            ),
        );
    }
}
fn protected(rig: &Rig) -> Protected {
    let mut vault = BTreeMap::new();
    vault_snapshot(&rig.state_dir, &rig.state_dir.join("vault"), &mut vault);
    Protected {
        core: core_snapshot(rig),
        vault,
        status: rig
            .shared
            .platform_custody
            .encrypted()
            .expect("encrypted manager missing")
            .status()
            .to_string(),
        smtp: must(
            rig.smtp_barrier(),
            "actual provider observation barrier failed",
        ),
    }
}
fn unchanged(rig: &Rig, watch: &KeyWatch, before: Protected) {
    assert_eq!(
        watch.drain(),
        0,
        "native forbidden caller accessed or altered configured key"
    );
    let after = protected(rig);
    assert!(
        after.core == before.core,
        "native forbidden caller mutated protected core authority/effects"
    );
    assert!(
        after.vault == before.vault,
        "native forbidden caller mutated vault/sidecars"
    );
    assert!(
        after.status == before.status,
        "native forbidden caller changed key-session state"
    );
    assert!(
        after.smtp == before.smtp,
        "native forbidden caller started provider traffic"
    );
}
fn registered_target(rig: &Rig, alias: &str) {
    must(
        rig.shared.store.register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: rig.state_dir.to_str().expect("fixture path encoding"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        }),
        "fixture target registration failed",
    );
}
fn register_probe(rig: &Rig, probe: &Probe) {
    registered_target(rig, "authority5-native-agent");
    let start = crate::peer::proc_starttime(probe.pid).expect("real probe starttime unavailable");
    let db = must(
        Connection::open_with_flags(
            &rig.core_db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ),
        "fixture pid writer failed",
    );
    assert_eq!(must(db.execute("UPDATE agents SET endpoint_kind='pty',pid=?1,pid_start=?2,generation='authority5-live',state='idle' WHERE alias='authority5-native-agent'",
        params![i64::from(probe.pid),i64::try_from(start).expect("starttime bounds")]),"real pid fixture publication failed"),1);
    assert!(db.is_autocommit(), "native actor fixture was not published");
}
fn assert_native_class(rig: &Rig, probe: &Probe, registered: bool) {
    assert!(
        crate::test_seam::asserted().is_none(),
        "native verification inherited a scoped assertion"
    );
    let who = must(
        rig.shared.connection_caller(probe.pid),
        "actual native caller derivation failed",
    );
    assert!(
        if registered {
            matches!(who,Who::Agent(ref a) if a=="authority5-native-agent")
        } else {
            matches!(who, Who::Unproven(_))
        },
        "real actor was not the required native caller class"
    );
}
fn deny_rpc(rig: &Rig, watch: &KeyWatch, probe: &mut Probe, method: &str, params: Value) {
    assert_eq!(
        watch.drain(),
        0,
        "watch had unexplained events before denial"
    );
    let before = protected(rig);
    let answer = probe.rpc(rig, method, params);
    assert_eq!(
        answer["ok"], false,
        "native caller acquired forbidden RPC authority"
    );
    assert_eq!(
        answer["error"]["kind"], "rejected",
        "transport/setup/storage failure is not native denial proof"
    );
    let reason = answer["error"]["message"]
        .as_str()
        .expect("native refusal message absent");
    assert!(
        reason.contains("operator action") || reason.contains("authority is connection-bound"),
        "RPC failure did not come from native caller/forged-identity gate"
    );
    unchanged(rig, watch, before);
}
fn deny_http(
    rig: &Rig,
    board: &Board,
    watch: &KeyWatch,
    probe: &mut Probe,
    verb: &str,
    path: &str,
    body: Value,
) {
    assert_eq!(
        watch.drain(),
        0,
        "watch had unexplained events before HTTP denial"
    );
    let before = protected(rig);
    let answer = board.http(rig, probe, verb, path, body);
    assert_eq!(
        answer["status"], 403,
        "authenticated native caller acquired authority or failed outside authority gate"
    );
    let body = answer["body"].as_str().expect("HTTP response absent");
    assert!(
        body.contains("operator") || body.contains("agent") || body.contains("caller_identity"),
        "HTTP refusal did not identify the caller/session authority gate"
    );
    unchanged(rig, watch, before);
}
fn create(rig: &Rig, account: &str, secret: &str) -> Value {
    json!({"provider":"smtp","account":account,"shape":"smtp","host":"localhost",
        "port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user","secret":secret,
        "sender":"sender@example.test","scopes":["email:send"],"accept_same_uid_risk":true})
}
fn cases(rig: &Rig, id: &str, revision: u64) -> Vec<(&'static str, Value, &'static str, String)> {
    let mut rotation = create(rig, "authority5", B);
    for key in ["provider", "account", "shape"] {
        rotation.as_object_mut().unwrap().remove(key);
    }
    rotation["connection_id"] = json!(id);
    rotation["expected_connection_id"] = json!(id);
    rotation["expected_revision"] = json!(revision);
    vec![
        (
            "connection_custody_initialize",
            json!({"accept_same_uid_risk":true}),
            "POST",
            "/api/connection-custody/initialize".into(),
        ),
        (
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
            "POST",
            "/api/connection-custody/unlock".into(),
        ),
        (
            "connection_custody_status",
            json!({}),
            "GET",
            "/api/connection-custody/status".into(),
        ),
        (
            "connection_custody_lock",
            json!({}),
            "POST",
            "/api/connection-custody/lock".into(),
        ),
        (
            "connection_create",
            create(rig, "authority5-forbidden", B),
            "POST",
            "/api/connections".into(),
        ),
        (
            "connection_rotate",
            rotation,
            "POST",
            format!("/api/connections/{id}/rotate"),
        ),
        (
            "connection_revoke",
            json!({"connection_id":id,"expected_connection_id":id,"expected_revision":revision}),
            "POST",
            format!("/api/connections/{id}/revoke"),
        ),
        // No browser grant endpoint exists in the inspected source. RPC only;
        // never count a made-up route's 404 as HTTP grant authority proof.
        (
            "platform_grant",
            json!({"agent":TARGET,"platform":"smtp","account":"authority5","scopes":["email:send"]}),
            "",
            String::new(),
        ),
    ]
}
fn http_body(mut params: Value, method: &str) -> Value {
    if matches!(method, "connection_rotate" | "connection_revoke") {
        params.as_object_mut().unwrap().remove("connection_id");
    }
    params
}

fn run_native_denials(http_only: bool) {
    // Never remove inherited identity/socket settings to make a check green.
    // The board uses ordinary client RPC: an ambient override could reach a
    // different daemon or synthesize relay identity. Refuse before rig startup.
    assert!(
        std::env::var_os("CADENCE_SOCKET").is_none(),
        "runner has an external socket override; isolated runner disposition required"
    );
    assert!(
        std::env::var_os("CADENCE_TEST_AS").is_none(),
        "runner has an asserted-caller environment; do not clear it"
    );
    assert!(
        crate::test_seam::asserted().is_none(),
        "fixture began under asserted caller scope"
    );
    let root = must(
        tempfile::Builder::new()
            .prefix("a5-")
            .tempdir_in(std::env::temp_dir()),
        "private isolated root failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "fixture root mode failed",
    );
    let rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "real isolated rig setup failed",
    );
    assert!(
        must(
            rig.state_dir.canonicalize(),
            "state canonicalization failed"
        )
        .starts_with(root.path()),
        "fixture root escaped"
    );
    registered_target(&rig, TARGET);
    registered_target(&rig, "authority5-control-target");
    let mut actors = [Probe::start(false), Probe::start(false), Probe::start(true)];
    register_probe(&rig, &actors[0]);
    assert_ne!(actors[0].pid, actors[1].pid);
    assert_eq!(actors[2].sid, actors[2].pid);
    for (index, actor) in actors.iter_mut().enumerate() {
        assert_native_class(&rig, actor, index == 0);
        eprintln!(
            "authority5 native role={} pid={} sid={} raw-request-no-assertion=true",
            ["registered", "unregistered", "setsid"][index],
            actor.pid,
            actor.sid
        );
        assert_eq!(
            actor.rpc(&rig, "health", json!({}))["ok"],
            true,
            "native raw RPC happy transport failed"
        );
    }
    let mut board = http_only.then(|| Board::start(&rig, root.path()));
    let key = root.path().join("keys/key");
    let watch = KeyWatch::start(&key);
    watch.calibrate(&key); // Synthetic same-UID read works; NOT protected isolation.
    let status = rig
        .shared
        .platform_custody
        .encrypted()
        .expect("real manager absent")
        .status();
    assert_eq!(
        status["isolation"], "none",
        "readable synthetic key was labelled isolated"
    );
    assert_eq!(
        status["protected"], false,
        "readable synthetic key was labelled protected"
    );
    let mut rpc_count = 0usize;
    let mut http_count = 0usize;

    // Cold store: denied initialize must not even open key/create vault.
    assert!(
        !rig.state_dir.join("vault").exists(),
        "cold fixture already had a vault"
    );
    for (index, actor) in actors.iter_mut().enumerate() {
        assert_native_class(&rig, actor, index == 0);
        for (method, params, verb, path) in cases(&rig, "not-yet-enrolled", 1).into_iter().take(4) {
            if !http_only {
                deny_rpc(&rig, &watch, actor, method, params.clone());
                rpc_count += 1;
            }
            if let Some(board) = &board {
                deny_http(&rig, board, &watch, actor, verb, &path, params);
                http_count += 1;
            }
        }
    }
    // Working lifecycle/enrollment/rotation/grant preconditions. These calls are
    // EXPLICIT SIMULATED SETUP, not evidence that this agent is an operator.
    setup_rpc(
        &rig,
        "connection_custody_initialize",
        json!({"accept_same_uid_risk":true}),
    );
    setup_rpc(
        &rig,
        "connection_custody_unlock",
        json!({"accept_same_uid_risk":true}),
    );
    let initial = setup_rpc(&rig, "connection_create", create(&rig, "authority5", A));
    let id = initial["connection"]["id"]
        .as_str()
        .expect("created Connection id absent")
        .to_string();
    assert!(
        must(rig.core_record(&id), "core observation failed")
            .custody
            .starts_with("encrypted-sqlite-v1:"),
        "legacy custody is not acceptance setup"
    );
    let rotate = cases(&rig, &id, 1)
        .into_iter()
        .find(|c| c.0 == "connection_rotate")
        .unwrap()
        .1;
    let mut rotate = rotate;
    rotate["secret"] = json!(A);
    setup_rpc(&rig, "connection_rotate", rotate);
    setup_rpc(
        &rig,
        "platform_grant",
        json!({"agent":"authority5-control-target","platform":"smtp","account":"authority5","scopes":["email:send"]}),
    );
    setup_rpc(
        &rig,
        "crm_smtp_bind",
        json!({"install_id":rig.install_id,"context_id":rig.context_id,"connection_id":id,"request_id":"authority5-bind"}),
    );
    let traffic = must(rig.smtp_barrier(), "SMTP happy barrier failed");
    setup_rpc(
        &rig,
        "crm_smtp_test_send",
        json!({"install_id":rig.install_id,"context_id":rig.context_id,"campaign_id":rig.campaign_id,"to_email":"recipient@example.test"}),
    );
    let seen = must(rig.smtp_barrier(), "SMTP happy completion barrier failed");
    assert_eq!(
        seen.accepted_messages,
        traffic.accepted_messages + 1,
        "SMTP setup lacked real successful DATA"
    );
    assert_eq!(
        seen.auth_a,
        traffic.auth_a + 1,
        "SMTP setup used wrong credential"
    );
    assert_eq!(
        seen.auth_b, traffic.auth_b,
        "SMTP setup unexpectedly used B"
    );
    assert!(
        watch.drain() > 0,
        "successful explicit setup did not exercise key access"
    );
    let revision =
        must(rig.core_record(&id), "current revision observation failed").credential_revision;
    assert_eq!(
        revision, 2,
        "rotation precondition did not publish revision 2"
    );
    for (index, actor) in actors.iter_mut().enumerate() {
        assert_native_class(&rig, actor, index == 0);
        for (method, params, verb, path) in cases(&rig, &id, revision) {
            if !http_only {
                deny_rpc(&rig, &watch, actor, method, params.clone());
                rpc_count += 1;
            }
            if let Some(board) = &board {
                if !path.is_empty() {
                    deny_http(
                        &rig,
                        board,
                        &watch,
                        actor,
                        verb,
                        &path,
                        http_body(params.clone(), method),
                    );
                    http_count += 1;
                }
            }
            // Forge one actor field on every callable native method. Redundant
            // native/strict-schema guards mean no single-guard-red claim here.
            let mut forged = params.clone();
            forged["actor"] = json!("operator");
            if !http_only {
                deny_rpc(&rig, &watch, actor, method, forged.clone());
                rpc_count += 1;
            }
            if method == "connection_custody_unlock" {
                if let Some(board) = &board {
                    deny_http(&rig, board, &watch, actor, verb, &path, forged);
                    http_count += 1;
                }
            }
            if matches!(
                method,
                "connection_custody_initialize"
                    | "connection_custody_unlock"
                    | "connection_create"
                    | "connection_rotate"
            ) {
                let mut forged = params.clone();
                forged["accept_same_uid_risk"] = json!("operator-approved");
                if !http_only {
                    deny_rpc(&rig, &watch, actor, method, forged.clone());
                    rpc_count += 1;
                }
                if let Some(board) = &board {
                    if !path.is_empty() {
                        deny_http(
                            &rig,
                            board,
                            &watch,
                            actor,
                            verb,
                            &path,
                            http_body(forged, method),
                        );
                        http_count += 1;
                    }
                }
            }
            if matches!(method, "connection_rotate" | "connection_revoke") {
                let mut forged = params.clone();
                forged["expected_connection_id"] = json!("foreign-incarnation");
                if !http_only {
                    deny_rpc(&rig, &watch, actor, method, forged.clone());
                    rpc_count += 1;
                }
                if let Some(board) = &board {
                    deny_http(
                        &rig,
                        board,
                        &watch,
                        actor,
                        verb,
                        &path,
                        http_body(forged, method),
                    );
                    http_count += 1;
                }
            }
        }
    }
    // Lock has an actual success control; denial must preserve locked state and
    // locked revoke's otherwise valid core-only authority retirement path.
    setup_rpc(&rig, "connection_custody_lock", json!({}));
    assert_eq!(watch.drain(), 0, "lock unexpectedly read key");
    for (index, actor) in actors.iter_mut().enumerate() {
        assert_native_class(&rig, actor, index == 0);
        for (method, params, verb, path) in cases(&rig, &id, revision).into_iter().filter(|c| {
            matches!(
                c.0,
                "connection_custody_unlock"
                    | "connection_custody_status"
                    | "connection_revoke"
                    | "platform_grant"
            )
        }) {
            if !http_only {
                deny_rpc(&rig, &watch, actor, method, params.clone());
                rpc_count += 1;
            }
            if let Some(board) = &board {
                if !path.is_empty() {
                    deny_http(
                        &rig,
                        board,
                        &watch,
                        actor,
                        verb,
                        &path,
                        http_body(params, method),
                    );
                    http_count += 1;
                }
            }
        }
    }
    // Final simulated happy revoke confirms target and expectations were valid;
    // it is not a native operator authority or full atomic-revoke proof.
    setup_rpc(
        &rig,
        "connection_revoke",
        json!({"connection_id":id,"expected_connection_id":id,"expected_revision":revision}),
    );
    assert!(
        must(
            rig.shared.store.connection_credential(&id),
            "final revoke observation failed"
        )
        .is_none(),
        "happy locked revoke did not retire target"
    );
    eprintln!("authority5 native denied RPC calls={rpc_count} authenticated HTTP calls={http_count}; no asserted identity on denial frames");
    assert_eq!(
        rpc_count,
        if http_only { 0 } else { 90 },
        "native RPC protected matrix silently changed"
    );
    assert_eq!(
        http_count,
        if http_only { 63 } else { 0 },
        "native HTTP protected matrix silently changed"
    );
    for actor in &mut actors {
        actor.finish();
    }
    if let Some(board) = &mut board {
        board.finish();
    }
    must(rig.finish(), "real rig cleanup failed");
}

#[test]
fn native_custody_rpc_refuses_real_forbidden_processes() {
    run_native_denials(false);
}

#[test]
fn native_custody_authenticated_http_refuses_real_forbidden_processes() {
    run_native_denials(true);
}
