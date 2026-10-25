// Independent owner cad1085-security; contract r2 SHA-256
// 3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8.
// One retained real-manager acceptance check, not native operator proof.
#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("custody6 requires admitted isolated Linux test-seam execution; never skip");
use super::Rig;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const A: &str = "YAwS5AyAesmZzdAuOBbBykufIyVNQQunwIqfTTWJlu4";
const B: &str = "Lc6BitPRSkgpooTCxJPhIuQaLtngFLkWw5MpFl5l4HY";
const ACCOUNT: &str = "custody6";
const WAIT: Duration = Duration::from_secs(20);
const FAULT: &str = include_str!("fault_probe.py");

fn must<T, E>(result: std::result::Result<T, E>, stage: &'static str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn bytes(path: &Path) -> Vec<u8> {
    must(std::fs::read(path), "fixture byte observation failed")
}
fn digest(path: &Path) -> String {
    hash(&bytes(path))
}
fn no_secret(text: &str) {
    assert!(
        !text.contains(A) && !text.contains(B),
        "synthetic credential leaked in result"
    );
}
fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    // EXPLICIT SYNTHETIC OPERATOR SETUP/CONTROL ONLY. Native authority proofs
    // belong to unchanged authority5-v5, not this lifecycle/fault test.
    let result = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, WAIT)
    });
    match &result {
        Ok(value) => no_secret(&value.to_string()),
        Err(error) => no_secret(&error.to_string()),
    }
    result
}
fn refuse(result: Result<Value>, codes: &[&str], stage: &'static str) -> Error {
    let error = match result {
        Err(e) => e,
        Ok(_) => panic!("{stage}"),
    };
    assert!(
        codes.contains(&error.code().unwrap_or("")),
        "refusal was not the expected behavioral guard"
    );
    error
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
    policy(rig);
}
fn lock(rig: &Rig) {
    must(
        rpc(rig, "connection_custody_lock", json!({})),
        "explicit lock failed",
    );
    closed(rig, false);
}
fn policy(rig: &Rig) {
    let p = must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_policy(),
        "actual policy observation failed",
    );
    assert_eq!(p.journal.to_lowercase(), "delete");
    assert_eq!(
        (
            p.synchronous,
            p.temp_store,
            p.trusted_schema,
            p.busy_timeout
        ),
        (3, 2, 0, 5000)
    );
    assert!(
        p.autocommit && !p.readonly && p.physical && p.has_key,
        "manager is not the actual idle writable vault"
    );
    let s = rig.shared.platform_custody.encrypted().unwrap().status();
    assert_eq!(s["unlocked"], true);
    assert_eq!(s["fenced"], false);
    assert_eq!(s["protected"], false);
    assert_eq!(s["isolation"], "none");
}
fn closed(rig: &Rig, fenced: bool) {
    let vault = rig.shared.platform_custody.encrypted().unwrap();
    let s = vault.status();
    assert_eq!(s["unlocked"], false);
    assert_eq!(s["fenced"], fenced);
    let p = must(vault.custody6_policy(), "closed manager observation failed");
    assert!(
        !p.physical && !p.has_key && p.autocommit,
        "lock/fence retained physical handle, key or transaction"
    );
}
fn create(rig: &Rig) -> Value {
    json!({"provider":"smtp","account":ACCOUNT,"shape":"smtp","host":"localhost",
    "port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user","secret":A,
    "sender":"sender@example.test","scopes":["email:send"],"accept_same_uid_risk":true})
}
fn rotation(rig: &Rig, id: &str, revision: u64) -> Value {
    json!({"connection_id":id,"expected_connection_id":id,"expected_revision":revision,"secret":B,"accept_same_uid_risk":true,
        "host":"localhost","port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user","sender":"sender@example.test"})
}
fn revoke(id: &str, revision: u64) -> Value {
    json!({"connection_id":id,"expected_connection_id":id,"expected_revision":revision})
}
fn send(rig: &Rig) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_test_send",
        json!({"install_id":rig.install_id,"context_id":rig.context_id,
        "campaign_id":rig.campaign_id,"to_email":"recipient@example.test"}),
    )
}
fn happy_a(rig: &Rig) {
    let before = must(rig.smtp_barrier(), "pre-positive real SMTP barrier failed");
    must(send(rig), "actual A TLS SMTP send failed");
    let after = must(rig.smtp_barrier(), "positive real SMTP drain failed");
    assert_eq!(after.tcp_sessions, before.tcp_sessions + 1);
    assert_eq!(after.accepted_messages, before.accepted_messages + 1);
    assert_eq!(after.auth_a, before.auth_a + 1);
    assert_eq!(after.auth_b, before.auth_b);
    assert_eq!(after.auth_other, before.auth_other);
}
fn no_send(rig: &Rig) {
    let before = must(rig.smtp_barrier(), "pre-refusal SMTP barrier failed");
    let error = match send(rig) {
        Err(e) => e,
        Ok(_) => panic!("locked/fenced/unsafe custody sent SMTP"),
    };
    assert!(
        matches!(
            error.code(),
            Some("locked" | "storage_unavailable" | "integrity_failure")
        ) || (error.code().is_none() && error.kind() == "rejected"),
        "SMTP refusal did not reach custody guard"
    );
    assert_eq!(
        must(rig.smtp_barrier(), "refusal SMTP drain failed"),
        before,
        "refused call contacted provider"
    );
}
fn same_authority(rig: &Rig, id: &str, core: &Value) {
    assert_eq!(
        must(rig.core_record(id), "core authority observation failed").to_json(),
        *core
    );
}

// Owned external PID: a faulting reader/locker/writer, never normal custody.
struct Fault {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}
impl Fault {
    fn start(path: &Path, mode: &str, extra: Option<Value>) -> Self {
        let mut child = must(
            Command::new("python3")
                .arg("-c")
                .arg(FAULT)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn(),
            "fault process spawn failed",
        );
        let input = child.stdin.take();
        let stdout = child.stdout.take().expect("fault stdout absent");
        let (tx, output) = mpsc::channel::<Value>();
        let reader = thread::spawn(move || {
            let mut lines = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let n = match lines.by_ref().take(8193).read_line(&mut line) {
                    Ok(n) => n,
                    Err(_) => break,
                };
                if n == 0 || n > 8192 || !line.ends_with('\n') {
                    break;
                }
                let Ok(value) = serde_json::from_str(&line) else {
                    break;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
        // Construct owner before any assertion so setup failures still reap it.
        let mut this = Self {
            child,
            input,
            output,
            reader: Some(reader),
        };
        let mut request = json!({"path":path,"mode":mode});
        if let Some(extra) = extra {
            request
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
        }
        must(
            writeln!(this.input.as_mut().unwrap(), "{request}"),
            "fault setup transport failed",
        );
        let ready = must(
            this.output.recv_timeout(WAIT),
            "fault setup deadline; not behavioral red",
        );
        assert_eq!(
            ready["ready"], true,
            "real fault setup failed; not behavioral red"
        );
        assert_eq!(ready["pid"].as_u64(), Some(this.child.id() as u64));
        assert_eq!(ready["mode"], mode);
        if mode == "shared" || mode == "hot" {
            assert_eq!(
                ready["in_transaction"], true,
                "fault process holds no real transaction"
            );
            assert_eq!(ready["journal"], "delete");
        }
        if mode == "recovery_shared" {
            assert_eq!(ready["native_shared"], true);
        }
        eprintln!(
            "custody6 real fault ready mode={mode} pid={} engine={}",
            this.child.id(),
            ready["engine"]
        );
        this
    }
    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = must(self.child.try_wait(), "fault wait failed") {
                return status;
            }
            assert!(Instant::now() < deadline, "fault shutdown deadline");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn release(mut self) {
        must(
            writeln!(self.input.as_mut().unwrap(), "{}", json!({"release":true})),
            "fault release transport failed",
        );
        self.input.take();
        let ack = must(
            self.output.recv_timeout(WAIT),
            "real fault release deadline",
        );
        assert_eq!(ack["released"], true);
        assert!(self.wait_exit().success(), "fault process failed release");
        if let Some(reader) = self.reader.take() {
            must(reader.join(), "fault observation thread failed");
        }
    }
    fn kill_for_hot_journal(mut self) {
        must(self.child.kill(), "owned crash fault SIGKILL failed");
        assert_eq!(
            self.wait_exit().signal(),
            Some(libc::SIGKILL),
            "fault child was not actually killed"
        );
        self.input.take();
        if let Some(reader) = self.reader.take() {
            must(reader.join(), "fault crash observation thread failed");
        }
    }
}
impl Drop for Fault {
    fn drop(&mut self) {
        // Only this exact Child. Never signal another lane/daemon/process.
        if self.reader.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.input.take();
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
        }
    }
}

// An actual new daemon event loop + sole manager against retained files.
// This is lifecycle teardown/reopen, not an OS daemon-process crash claim.
fn reopen(old: Rig, root: &Path) -> Rig {
    let state = old.state_dir.clone();
    let core = old.core_db_path.clone();
    let port = old.smtp_port;
    let install_id = old.install_id.clone();
    let context_id = old.context_id.clone();
    let campaign_id = old.campaign_id.clone();
    must(old.finish(), "old sole Rig drain/drop failed"); // consumes and drops Shared/key/manager
    let mut owner = super::Owner::new();
    let listener = must(
        TcpListener::bind(("127.0.0.1", port)),
        "retained SMTP endpoint rebind failed",
    );
    must(
        listener.set_nonblocking(true),
        "reopen SMTP nonblocking failed",
    );
    let tls = must(super::smtp_config(), "reopen TLS config failed");
    let (barriers, receiver) = mpsc::channel();
    let counts = Arc::new(super::LiveCounts::default());
    owner.smtp_thread = Some(thread::spawn({
        let stop = owner.smtp_stop.clone();
        move || {
            super::smtp_loop(
                listener,
                tls,
                counts,
                Arc::new(A.as_bytes().to_vec()),
                Arc::new(B.as_bytes().to_vec()),
                stop,
                receiver,
            )
        }
    }));
    let observer = Arc::new(Mutex::new(None));
    let mut opts = crate::daemon::ServeOptions {
        custody_acceptance_shared: Some(observer.clone()),
        stop: Some(owner.daemon_stop.clone()),
        test_seam: true,
        smtp_test_ca_pem: Some(super::CA_PEM.to_vec()),
        ..Default::default()
    };
    opts.provider_env.set(
        "CADENCE_PM_DIR",
        root.join("pm").to_str().expect("fixture path encoding"),
    );
    opts.encrypted_custody = Some(crate::platform::encrypted_sqlite::EncryptedCustodyConfig {
        key_file: root.join("keys/key"),
        vault_dir: state.join("vault"),
    });
    opts.auto_stop = Some(crate::daemon::AutoStopSetting::off());
    opts.report_router = Some(0);
    crate::platform::local::register(&state, &mut opts);
    crate::platform::smtp::attach(&mut opts);
    owner.daemon_thread = Some(thread::spawn({
        let state = state.clone();
        move || crate::daemon::serve_with(&state, opts)
    }));
    must(
        super::wait_socket(&state, &owner, WAIT),
        "reopened actual daemon readiness failed",
    );
    let shared = must(observer.lock(), "reopen observer poisoned")
        .clone()
        .expect("reopen real Shared absent");
    let workspace_id = must(
        shared.store.connection_workspace_id(),
        "reopen workspace absent",
    );
    Rig {
        state_dir: state,
        core_db_path: core,
        workspace_id,
        smtp_port: port,
        install_id,
        context_id,
        campaign_id,
        owner: Mutex::new(owner),
        barriers,
        shared,
    }
}

struct KeyWatch(OwnedFd);
impl KeyWatch {
    fn start(key: &Path) -> Self {
        use std::os::unix::ffi::OsStrExt;
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(raw >= 0, "key inotify unavailable");
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let path = must(
            CString::new(key.as_os_str().as_bytes()),
            "watch path invalid",
        );
        let watch = unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_OPEN | libc::IN_ACCESS,
            )
        };
        assert!(watch >= 0, "actual synthetic key watch failed");
        Self(fd)
    }
    fn drain(&self) -> usize {
        let mut count = 0;
        loop {
            let mut buffer = [0u8; 8192];
            let n =
                unsafe { libc::read(self.0.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 {
                assert_eq!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock
                );
                return count;
            }
            assert!(n > 0);
            let mut at = 0;
            while at + std::mem::size_of::<libc::inotify_event>() <= n as usize {
                let event = unsafe {
                    std::ptr::read_unaligned(buffer.as_ptr().add(at).cast::<libc::inotify_event>())
                };
                assert_eq!(
                    event.mask & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED),
                    0,
                    "key observation lost events/watch"
                );
                at += std::mem::size_of::<libc::inotify_event>() + event.len as usize;
                if event.mask & (libc::IN_OPEN | libc::IN_ACCESS) != 0 {
                    count += 1;
                }
            }
            assert_eq!(at, n as usize);
        }
    }
}
fn replace_key(path: &Path, value: &[u8]) {
    let mut file = must(
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .mode(0o600)
            .open(path),
        "fixture key write failed",
    );
    must(file.write_all(value), "fixture key bytes write failed");
    must(file.sync_all(), "fixture key sync failed");
    must(
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)),
        "fixture key mode restore failed",
    );
}
fn bad_unlock(rig: &Rig, id: &str, core: &Value, db: &Path, original: &str, codes: &[&str]) {
    refuse(
        rpc(
            rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
        ),
        codes,
        "unsafe/wrong key unlocked custody",
    );
    closed(rig, true);
    same_authority(rig, id, core);
    assert_eq!(
        digest(db),
        original,
        "failed unlock changed the authenticated vault"
    );
    no_send(rig);
}

#[test]
fn connection_custody_key_session_recovery_and_commit_busy_are_fail_closed() {
    let root = must(
        tempfile::Builder::new()
            .prefix("c6-")
            .tempdir_in(std::env::temp_dir()),
        "isolated custody6 root failed",
    );
    assert!(
        root.path().starts_with(std::env::temp_dir()),
        "root escapes admitted TMPDIR"
    );
    let mut rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "real sole Rig setup failed",
    );
    let key = root.path().join("keys/key");
    let db = rig.state_dir.join("vault/custody.sqlite3");
    assert!(!db.exists(), "cold fixture already opened/created custody");
    let watch = KeyWatch::start(&key);
    let saved_key = zeroize::Zeroizing::new(bytes(&key));
    assert_eq!(saved_key.len(), 32);
    assert!(
        watch.drain() > 0,
        "key observer failed genuine same-UID access calibration"
    );
    closed(&rig, true);
    assert_eq!(watch.drain(), 0, "cold status read the key");
    refuse(
        rpc(&rig, "connection_custody_initialize", json!({})),
        &["custody_unprotected"],
        "init without explicit risk succeeded",
    );
    assert!(!db.exists());
    assert_eq!(watch.drain(), 0, "risk refusal opened key");
    must(
        rpc(
            &rig,
            "connection_custody_initialize",
            json!({"accept_same_uid_risk":true}),
        ),
        "initialization failed",
    );
    closed(&rig, false);
    assert!(
        watch.drain() > 0,
        "initialize did not read actual configured key"
    );
    refuse(
        rpc(&rig, "connection_custody_unlock", json!({})),
        &["custody_unprotected"],
        "unlock without explicit risk succeeded",
    );
    assert_eq!(watch.drain(), 0);
    unlock(&rig);
    assert!(watch.drain() > 0, "unlock did not read actual key");
    drop(watch);
    let created = must(
        rpc(&rig, "connection_create", create(&rig)),
        "actual encrypted enrollment failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("enrollment id absent")
        .to_string();
    let record = must(
        rig.core_record(&id),
        "initial actual core authority missing",
    );
    assert_eq!(record.credential_revision, 1);
    let core = record.to_json();
    let rows = must(
        rig.vault_rows(&id),
        "initial sole-manager row observation failed",
    );
    assert_eq!(rows.len(), 1);
    must(
        rpc(
            &rig,
            "crm_smtp_bind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,"connection_id":id,"request_id":"c6-bind"}),
        ),
        "actual SMTP binding failed",
    );
    happy_a(&rig);
    eprintln!("custody6 stage=real-control complete");

    // Real C contention: immediate busy, no deferred lock/revoke/rotation.
    let before = must(rig.smtp_barrier(), "contention SMTP barrier failed");
    {
        let _c = must(
            rig.shared.custody_operation(),
            "actual C acquisition failed",
        );
        for (method, params) in [
            ("connection_custody_lock", json!({})),
            ("connection_revoke", revoke(&id, 1)),
            ("connection_rotate", rotation(&rig, &id, 1)),
        ] {
            let began = Instant::now();
            refuse(
                rpc(&rig, method, params),
                &["busy"],
                "C contender was accepted",
            );
            assert!(
                began.elapsed() < Duration::from_secs(2),
                "C busy was not immediate"
            );
        }
    }
    policy(&rig);
    same_authority(&rig, &id, &core);
    assert_eq!(must(rig.vault_rows(&id), "contention rows failed"), rows);
    assert_eq!(must(rig.smtp_barrier(), "contention drain failed"), before);
    happy_a(&rig); // Released contention did not schedule a delayed lock/revoke.
    lock(&rig);
    no_send(&rig);
    unlock(&rig);

    // COMMIT actually reaches BUSY after BEGIN + INSERT under an external
    // SHARED reader. The returned known rollback must be idle, no new row/core.
    let before = must(rig.smtp_barrier(), "commit-busy SMTP barrier failed");
    let blocker = Fault::start(&db, "shared", None);
    let probe = must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_probe(false),
        "real SQL phase probe failed",
    );
    let began = Instant::now();
    refuse(
        rpc(&rig, "connection_rotate", rotation(&rig, &id, 1)),
        &["busy"],
        "COMMIT busy published rotation",
    );
    assert!(
        began.elapsed() >= Duration::from_secs(4),
        "real configured busy timeout was not exercised"
    );
    let sql = probe.counts();
    assert_eq!(
        (sql.begin, sql.insert, sql.commit, sql.rollback),
        (1, 1, 1, 1),
        "refusal did not reach real INSERT/COMMIT/explicit ROLLBACK"
    );
    policy(&rig);
    must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_clear_probe(),
        "SQL probe removal failed",
    );
    same_authority(&rig, &id, &core);
    assert_eq!(
        must(rig.vault_rows(&id), "rolled-back row inspection failed"),
        rows
    );
    assert_eq!(must(rig.smtp_barrier(), "commit-busy drain failed"), before);
    blocker.release();
    happy_a(&rig);
    eprintln!("custody6 stage=real-commit-busy-rollback complete");

    // Same genuine COMMIT Busy, but SQLite itself denies the required
    // ROLLBACK. Unknown outcome MUST close connection, erase session, fence.
    let blocker = Fault::start(&db, "shared", None);
    let probe = must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_probe(true),
        "rollback fault installation failed",
    );
    refuse(
        rpc(&rig, "connection_rotate", rotation(&rig, &id, 1)),
        &["outcome_unknown"],
        "failed real rollback was treated as known/committed",
    );
    let sql = probe.counts();
    assert_eq!(
        (sql.begin, sql.insert, sql.commit, sql.rollback),
        (1, 1, 1, 1)
    );
    closed(&rig, true);
    same_authority(&rig, &id, &core);
    no_send(&rig);
    refuse(
        rpc(&rig, "connection_rotate", rotation(&rig, &id, 1)),
        &["storage_unavailable", "locked"],
        "fenced writer continued",
    );
    // Manager callbacks are independently denied, even with a complete valid
    // authoritative tuple supplied (the RPC can stop earlier at no-key admission).
    {
        let _c = must(
            rig.shared.custody_operation(),
            "fenced callback C acquisition failed",
        );
        let vault = rig.shared.platform_custody.encrypted().unwrap();
        let expected = crate::platform::encrypted_sqlite::ExpectedIdentity {
            workspace_id: rig.workspace_id.clone(),
            connection_id: id.clone(),
            provider: record.platform.clone(),
            account: record.account.clone(),
            credential_revision: record.credential_revision,
            exchange: record.exchange.clone(),
            key_id: crate::secret::fingerprint_key_id(&saved_key),
        };
        match vault.read_exact(&record.custody, &expected) {
            Err(error) => assert_eq!(error.code(), Some("storage_unavailable")),
            Ok(_) => panic!("fenced exact-read callback decrypted"),
        }
        match vault.write_row(&expected, B.as_bytes()) {
            Err(error) => assert_eq!(error.code(), Some("storage_unavailable")),
            Ok(_) => panic!("fenced write callback executed"),
        }
    }
    assert_eq!(probe.counts(), sql, "fenced calls touched old SQL handle");
    blocker.release();
    unlock(&rig); // new operator-directed physical open/header confirmation
    assert_eq!(
        must(rig.vault_rows(&id), "reopened fenced rows failed"),
        rows
    );
    happy_a(&rig);
    eprintln!("custody6 stage=actual-rollback-denial-fence complete");

    // Real PRAGMA drift on sole handle must fence before decrypt/provider.
    must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_downgrade_sync(),
        "actual synchronous drift fault failed",
    );
    let p = must(
        rig.shared
            .platform_custody
            .encrypted()
            .unwrap()
            .custody6_policy(),
        "drift observation failed",
    );
    assert_eq!(
        p.synchronous, 0,
        "fault did not change actual SQLite policy"
    );
    no_send(&rig);
    closed(&rig, true);
    unlock(&rig);
    happy_a(&rig);

    // Bad external keys do not modify header/records or resume provider.
    lock(&rig);
    let original = digest(&db);
    let wrong = zeroize::Zeroizing::new(vec![0x9b; 32]);
    for value in [wrong.as_slice(), &saved_key[..31], &[0x9b; 33][..]] {
        replace_key(&key, value);
        bad_unlock(
            &rig,
            &id,
            &core,
            &db,
            &original,
            &["key_unavailable", "integrity_failure"],
        );
        replace_key(&key, &saved_key);
    }
    let parked = root.path().join("keys/parked-key");
    must(std::fs::rename(&key, &parked), "missing-key setup failed");
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::rename(&parked, &key),
        "missing-key restoration failed",
    );
    must(
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)),
        "unsafe key mode setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)),
        "key mode restoration failed",
    );
    must(std::fs::hard_link(&key, &parked), "key alias setup failed");
    assert_eq!(
        must(std::fs::metadata(&key), "key nlink observation failed").nlink(),
        2
    );
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::remove_file(&parked),
        "owned key hardlink cleanup failed",
    );
    must(std::fs::rename(&key, &parked), "key symlink parking failed");
    must(
        std::os::unix::fs::symlink(&parked, &key),
        "key final symlink setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::remove_file(&key),
        "owned key symlink cleanup failed",
    );
    must(
        std::fs::rename(&parked, &key),
        "key symlink restoration failed",
    );
    let parent = root.path().join("keys");
    must(
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)),
        "unsafe key parent setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)),
        "key parent restoration failed",
    );

    // Final FIFO and symlinked ancestor are real objects, never a fake path flag.
    must(std::fs::rename(&key, &parked), "FIFO key parking failed");
    {
        use std::os::unix::ffi::OsStrExt;
        let path = must(
            CString::new(key.as_os_str().as_bytes()),
            "FIFO path encoding failed",
        );
        assert_eq!(
            unsafe { libc::mkfifo(path.as_ptr(), 0o600) },
            0,
            "owned FIFO creation failed"
        );
    }
    let began = Instant::now();
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "unsafe key FIFO blocked admission"
    );
    must(std::fs::remove_file(&key), "owned key FIFO cleanup failed");
    must(
        std::fs::rename(&parked, &key),
        "FIFO key restoration failed",
    );
    let parked_parent = root.path().join("retained-keys");
    must(
        std::fs::rename(&parent, &parked_parent),
        "key ancestor parking failed",
    );
    must(
        std::os::unix::fs::symlink(&parked_parent, &parent),
        "key ancestor symlink setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &original, &["key_unavailable"]);
    must(
        std::fs::remove_file(&parent),
        "owned key ancestor symlink cleanup failed",
    );
    must(
        std::fs::rename(&parked_parent, &parent),
        "key ancestor restoration failed",
    );

    // Public key-id equality cannot substitute for actual AEAD key confirmation.
    let original_id = crate::secret::fingerprint_key_id(&saved_key);
    let wrong_id = crate::secret::fingerprint_key_id(&wrong);
    assert_ne!(original_id, wrong_id);
    let id_hex = |id: &[u8]| id.iter().map(|v| format!("{v:02x}")).collect::<String>();
    replace_key(&key, &wrong);
    Fault::start(&db, "header", Some(json!({"key_id":id_hex(&wrong_id)}))).release();
    let tampered = digest(&db);
    bad_unlock(&rig, &id, &core, &db, &tampered, &["integrity_failure"]);
    Fault::start(&db, "header", Some(json!({"key_id":id_hex(&original_id)}))).release();
    replace_key(&key, &saved_key);
    unlock(&rig);
    assert_eq!(
        must(rig.vault_rows(&id), "key fault restored rows failed"),
        rows
    );
    happy_a(&rig);
    eprintln!("custody6 stage=real-key-admission-and-aead-confirmation complete");

    // Existing-only RW open: absent vault cannot be recreated from core refs.
    lock(&rig);
    let vault_dir = rig.state_dir.join("vault");
    let retained = root.path().join("retained-vault");
    must(
        std::fs::rename(&vault_dir, &retained),
        "vault-only absence setup failed",
    );
    refuse(
        rpc(
            &rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
        ),
        &["invalid_store"],
        "missing referenced vault was created/opened",
    );
    assert!(!vault_dir.exists(), "unlock used CREATE for missing vault");
    closed(&rig, true);
    same_authority(&rig, &id, &core);
    no_send(&rig);
    must(
        std::fs::rename(&retained, &vault_dir),
        "owned retained-vault restoration failed",
    );
    // Owner-mode/nlink/final-symlink checks precede actual SQLite open.
    let before_db = digest(&db);
    must(
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)),
        "unsafe DB mode setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &before_db, &["invalid_store"]);
    must(
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)),
        "DB mode restoration failed",
    );
    let parked_db = vault_dir.join("owned-parked-db");
    must(
        std::fs::hard_link(&db, &parked_db),
        "DB hardlink setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &before_db, &["invalid_store"]);
    must(
        std::fs::remove_file(&parked_db),
        "owned DB hardlink cleanup failed",
    );
    must(
        std::fs::rename(&db, &parked_db),
        "DB symlink parking failed",
    );
    must(
        std::os::unix::fs::symlink(&parked_db, &db),
        "DB final symlink setup failed",
    );
    bad_unlock(&rig, &id, &core, &db, &before_db, &["invalid_store"]);
    must(
        std::fs::remove_file(&db),
        "owned DB final symlink cleanup failed",
    );
    must(
        std::fs::rename(&parked_db, &db),
        "DB symlink restoration failed",
    );
    for suffix in ["-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", db.display()));
        let mut file = must(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path),
            "unsupported sidecar setup failed",
        );
        must(
            file.write_all(b"owner-custody6-sidecar"),
            "sidecar bytes failed",
        );
        must(file.sync_all(), "sidecar sync failed");
        drop(file);
        let before_db = digest(&db);
        let before_sidecar = digest(&path);
        refuse(
            rpc(
                &rig,
                "connection_custody_unlock",
                json!({"accept_same_uid_risk":true}),
            ),
            &["unsupported_format"],
            "preexisting WAL/SHM was admitted",
        );
        assert_eq!(digest(&db), before_db);
        assert_eq!(
            digest(&path),
            before_sidecar,
            "unsupported sidecar silently cleaned/converted"
        );
        closed(&rig, true);
        must(std::fs::remove_file(&path), "owned sidecar cleanup failed");
    }
    let original_db = bytes(&db);
    let mut wal_header = original_db.clone();
    wal_header[18] = 2;
    wal_header[19] = 2;
    must(
        std::fs::write(&db, &wal_header),
        "WAL header fault setup failed",
    );
    refuse(
        rpc(
            &rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
        ),
        &["unsupported_format"],
        "WAL header silently converted",
    );
    assert!(
        bytes(&db) == wal_header,
        "WAL-header refusal changed vault bytes"
    );
    must(
        std::fs::write(&db, &original_db),
        "WAL header restoration failed",
    );
    unlock(&rig);
    happy_a(&rig);

    // Actual SIGKILL of owned SQLite writer leaves a real spilled hot journal.
    // A native SHARED lock makes recovery unavailable; no successful unlock,
    // deletion or provider access until release and explicit fresh unlock.
    lock(&rig);
    Fault::start(&db, "hot", None).kill_for_hot_journal();
    let journal = PathBuf::from(format!("{}-journal", db.display()));
    let hot = digest(&journal);
    assert_eq!(
        &bytes(&journal)[..8],
        &[0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7]
    );
    let blocker = Fault::start(&db, "recovery_shared", None);
    refuse(
        rpc(
            &rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk":true}),
        ),
        &["busy", "storage_unavailable", "invalid_store"],
        "blocked hot recovery unlocked custody",
    );
    closed(&rig, true);
    same_authority(&rig, &id, &core);
    assert_eq!(
        digest(&journal),
        hot,
        "failed recovery deleted/rewrote hot journal"
    );
    no_send(&rig);
    blocker.release();
    unlock(&rig);
    assert!(
        !journal.exists(),
        "successful DELETE recovery left hot journal"
    );
    assert_eq!(
        must(rig.vault_rows(&id), "real recovered rows failed"),
        rows
    );
    happy_a(&rig);
    eprintln!("custody6 stage=real-hot-journal-recovery complete");

    // Teardown/reopen remains locked despite retained usable external key/core.
    let watch = KeyWatch::start(&key);
    let mut calibration = zeroize::Zeroizing::new([0u8; 32]);
    must(
        must(
            std::fs::File::open(&key),
            "reopen watcher calibration open failed",
        )
        .read_exact(calibration.as_mut()),
        "reopen watcher calibration read failed",
    );
    assert!(watch.drain() > 0);
    rig = reopen(rig, root.path());
    closed(&rig, true);
    assert_eq!(
        watch.drain(),
        0,
        "daemon startup/status automatically read key/unlocked"
    );
    same_authority(&rig, &id, &core);
    no_send(&rig);
    assert_eq!(watch.drain(), 0, "locked provider denial read external key");
    unlock(&rig);
    assert!(
        watch.drain() > 0,
        "explicit post-reopen unlock did not read actual key"
    );
    happy_a(&rig);

    // Core-only revoke remains available while locked, without key or vault SQL.
    lock(&rig);
    assert_eq!(watch.drain(), 0, "lock reread key");
    let vault_before = digest(&db);
    must(
        rpc(&rig, "connection_revoke", revoke(&id, 1)),
        "locked exact revoke failed",
    );
    assert!(must(
        rig.shared.store.connection_credential(&id),
        "locked revoke authority observation failed"
    )
    .is_none());
    assert_eq!(watch.drain(), 0, "locked revoke opened key");
    assert_eq!(digest(&db), vault_before, "locked revoke rewrote vault");
    closed(&rig, false);
    eprintln!("custody6 stage=locked-start-and-core-only-revoke complete");
    must(rig.finish(), "final owned Rig drain failed");
}
