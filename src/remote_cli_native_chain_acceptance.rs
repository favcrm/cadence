// CAD-1434 / AOS-243 controlled native chain, included in remote_cli::tests.
// B=https://lc.cadencecloud.app; I=https://issuer.board.localhost:P.
// Physical P is Node HTTPS, N is native HTTP (internal TLS termination only).
// The controller fetches the REAL AOS JWKS over verified TLS to prime the
// normal cache; native requests verify with that cache, not a fresh TLS fetch.
// The existing daemon test seam authenticates only the internal board peer.
// Actual signed envelope, grant and native wiki ACL checks remain unchanged.

use super::*;
use crate::adapter::ProviderEnv;
use crate::issuer_transport::test_fixture::{self, Fixture};
use crate::test_seam::{scoped, Asserted, Seam};
use crate::{client, daemon, issue::Pm, ui};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use ring::signature::{Ed25519KeyPair, KeyPair};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::JoinHandle;

const BOARD: &str = "https://lc.cadencecloud.app";
const HOST: &str = "lc.cadencecloud.app";
const SLUG: &str = "lc";
const WORKSPACE: &str = "lc_ws";
const KID: &str = "lc-native-key";
const READY_WAIT: Duration = Duration::from_secs(120);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    owner: Value,
    member: Value,
    owner_id: String,
    member_id: String,
    issuer: String,
    audience: String,
}

// Isolate only ordinary filesystem environment; never clear native identity.
struct Environment(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl Environment {
    fn isolate(root: &Path) -> Self {
        let mut previous = Vec::new();
        for (name, relative) in [
            ("HOME", "home"),
            ("XDG_STATE_HOME", "xdg"),
            ("XDG_CONFIG_HOME", "home/config"),
            ("XDG_DATA_HOME", "home/data"),
            ("TMPDIR", ""),
            ("CADENCE_SUITE_LOCK", "locks/suite"),
        ] {
            previous.push((name, std::env::var_os(name)));
            std::env::set_var(name, root.join(relative));
        }
        Self(previous)
    }
}
impl Drop for Environment {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..) {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

struct Rig {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<crate::Result<()>>>,
    state: PathBuf,
    pm: PathBuf,
    port: u16,
    seam: String,
}
impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}
impl Rig {
    fn start(root: &Path, issuer: &str) -> Self {
        let pm_dir = root.join("pm");
        Pm::init(&pm_dir).unwrap();
        let state = root.join("s");
        let stop = Arc::new(AtomicBool::new(false));
        let mut rig = Self {
            stop,
            threads: Vec::new(),
            state: state.clone(),
            pm: pm_dir.clone(),
            port: 0,
            seam: String::new(),
        };
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let opts = daemon::ServeOptions {
            provider_env,
            stop: Some(rig.stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let run_state = state.clone();
        rig.threads.push(std::thread::spawn(move || {
            daemon::serve_with(&run_state, opts)
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                if let Some(token) = Seam::token_at(&state) {
                    rig.seam = token;
                    break;
                }
            }
            assert!(
                !rig.threads[0].is_finished(),
                "isolated daemon exited before readiness"
            );
            assert!(
                Instant::now() < deadline,
                "isolated daemon readiness timed out"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // The caller chooses P first; exclude it while native UI chooses N.
        rig.port = free_port(&[issuer.rsplit(':').next().unwrap().parse().unwrap()]);
        let (ready, wait) = std::sync::mpsc::channel();
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: rig.port,
            allow_hosts: vec![HOST.into()],
            board_public_only: true,
            public: Some(ui::PublicBoard {
                host: HOST.into(),
                issuer: issuer.into(),
                company: WORKSPACE.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            test_seam: true,
            stop: Some(rig.stop.clone()),
            startup: Some(ready),
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        rig.threads.push(std::thread::spawn(move || {
            ui::serve(&state, &pm_dir, &opts)
        }));
        wait.recv_timeout(Duration::from_secs(30))
            .expect("native board readiness timed out")
            .unwrap();
        rig
    }
    fn rpc(&self, verb: &str, args: Value) -> Value {
        scoped(Asserted::Operator, || client::rpc(&self.state, verb, args)).unwrap()
    }
    fn snapshot(&self) -> (Vec<u8>, Vec<(PathBuf, Vec<u8>)>) {
        fn walk(root: &Path, path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
            if !path.exists() {
                return;
            }
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(root, &path, out);
                } else {
                    out.push((
                        path.strip_prefix(root).unwrap().to_owned(),
                        fs::read(path).unwrap(),
                    ));
                }
            }
        }
        let head = crate::proc::run_bounded(
            Command::new("git")
                .arg("-C")
                .arg(&self.pm)
                .args(["rev-parse", "HEAD"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(head.status.success());
        let vault = crate::wiki::vault_dir(&Pm::at(&self.pm).unwrap()).unwrap();
        let mut files = Vec::new();
        walk(&vault, &vault, &mut files);
        walk(&self.state, &self.state.join("wiki-uploads"), &mut files);
        files.sort();
        (head.stdout, files)
    }
}

fn free_port(excluded: &[u16]) -> u16 {
    (3110..=3199)
        .find(|p| !excluded.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no free isolated fixture port in 3110..3199")
}
fn private_write(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
fn openssl(command: &mut Command) {
    let result = crate::proc::run_bounded(command, Duration::from_secs(15)).unwrap();
    assert!(
        result.status.success(),
        "private fixture PKI generation failed"
    );
}
fn pki(root: &Path) -> (Vec<u8>, PathBuf, PathBuf) {
    let ca = root.join("ca.pem");
    let ca_key = root.join("ca.key");
    let key = root.join("tls.key");
    let csr = root.join("tls.csr");
    let cert = root.join("tls.pem");
    let ext = root.join("tls.ext");
    private_write(&ext, b"subjectAltName=DNS:lc.cadencecloud.app,DNS:issuer.board.localhost\nbasicConstraints=CA:FALSE\n");
    openssl(
        Command::new("openssl")
            .args([
                "req",
                "-x509",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-days",
                "1",
                "-subj",
                "/CN=CAD1434 private fixture CA",
            ])
            .arg("-keyout")
            .arg(&ca_key)
            .arg("-out")
            .arg(&ca),
    );
    openssl(
        Command::new("openssl")
            .args([
                "req",
                "-newkey",
                "ec",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-nodes",
                "-subj",
                "/CN=lc.cadencecloud.app",
            ])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&csr),
    );
    openssl(
        Command::new("openssl")
            .args(["x509", "-req", "-days", "1"])
            .arg("-in")
            .arg(&csr)
            .arg("-CA")
            .arg(&ca)
            .arg("-CAkey")
            .arg(&ca_key)
            .arg("-CAcreateserial")
            .arg("-extfile")
            .arg(&ext)
            .arg("-out")
            .arg(&cert),
    );
    for path in [&ca_key, &key] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut reader = std::io::BufReader::new(File::open(ca).unwrap());
    let ca_der = rustls_pemfile::certs(&mut reader)
        .next()
        .unwrap()
        .unwrap()
        .to_vec();
    (ca_der, cert, key)
}

// One owned process group covers Node, its Vitest worker and local workerd.
// Panic, startup failure and successful finish all tear down owned children.
struct Driver {
    child: Child,
}
impl Driver {
    fn status(&self) -> Option<bool> {
        // Observe without reaping: the owned leader's PID remains reserved
        // until Drop signals the group, including workers after an early exit.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        assert_eq!(result, 0, "cannot observe owned driver status");
        if unsafe { info.si_pid() } == 0 {
            return None;
        }
        Some(info.si_code == libc::CLD_EXITED && unsafe { info.si_status() } == 0)
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}
fn spawn_driver(root: &Path, port: u16, rig: &Rig, cert: &Path, key: &Path, nonce: &str) -> Driver {
    let api = PathBuf::from(
        std::env::var_os("CAD1434_AOS_API_DIR")
            .expect("controlled run needs explicit CAD1434_AOS_API_DIR"),
    );
    assert!(
        api.is_absolute() && api.join("vitest.config.ts").is_file(),
        "invalid explicit AOS API directory"
    );
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let keys = json!([{"kty":"OKP", "crv":"Ed25519", "kid":KID,
        "x":URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()), "d":URL_SAFE_NO_PAD.encode(seed)}])
    .to_string();
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("driver.log"))
        .unwrap();
    Driver {
        child: Command::new("node")
            .current_dir(api)
            .args([
                "node_modules/vitest/vitest.mjs",
                "run",
                "src/hosted-cadence/native-chain.acceptance.test.ts",
                "--pool=forks",
                "--maxWorkers=1",
                "--no-file-parallelism",
            ])
            .env("CAD1434_NATIVE_CHAIN", "1")
            .env("CAD1434_BRIDGE_PORT", port.to_string())
            .env(
                "CAD1434_NATIVE_ORIGIN",
                format!("http://127.0.0.1:{}", rig.port),
            )
            .env("CAD1434_BOARD_KEYS", keys)
            .env("CAD1434_TLS_CERT", cert)
            .env("CAD1434_TLS_KEY", key)
            .env("CAD1434_TOKEN_OUT", root.join("tokens.json"))
            .env("CAD1434_CONTROL_NONCE", nonce)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("spawn controlled AOS peer"),
    }
}
fn wait_ready(driver: &mut Driver, path: &Path) -> Ready {
    let deadline = Instant::now() + READY_WAIT;
    loop {
        if let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            let meta = file.metadata().unwrap();
            assert!(meta.is_file() && meta.mode() & 0o077 == 0 && meta.len() <= 32768);
            let mut bytes = Vec::new();
            file.take(32769).read_to_end(&mut bytes).unwrap();
            return serde_json::from_slice(&bytes).expect("invalid private ready contract");
        }
        assert!(
            driver.status().is_none(),
            "AOS peer exited before readiness (private driver.log)"
        );
        assert!(Instant::now() < deadline, "AOS peer readiness timed out");
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn control(port: u16, nonce: &str, op: &str) -> u16 {
    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .proxy(None),
    );
    let response = agent
        .post(format!(
            "https://issuer.board.localhost:{port}/__cad1434_fixture/control"
        ))
        .header("x-cad1434-fixture-nonce", nonce)
        .send_json(json!({"op":op}))
        .unwrap();
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    response
        .into_body()
        .as_reader()
        .take(1025)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() <= 1024, "control response exceeds limit");
    if status == 200 {
        assert!(serde_json::from_slice::<Value>(&bytes).unwrap()["ok"] == true);
    }
    status
}
fn call(target: &RemoteTarget, dir: &Path, action: &str, args: Value) -> Result<Value> {
    match call_verb_in(
        target,
        action,
        args.as_object().unwrap().clone(),
        60,
        dir,
        |_| {},
        std::thread::sleep,
    )? {
        RemoteAnswer::Ok(value) => Ok(value),
        RemoteAnswer::Conflict(_) => panic!("unexpected native conflict"),
    }
}
fn expire_client(dir: &Path, target: &RemoteTarget) {
    let mut credential = load_credential(dir, target).unwrap();
    credential.expires_at = now().unwrap() - 1;
    save_credential(dir, SLUG, &credential).unwrap();
}

// This is the existing cfg(test, feature="test-seam") harness, not the
// installed CLI binary. Private piped stdin is its only fixture input.
const CLIENT_HELPER: &str =
    "remote_cli::tests::native_chain_acceptance::native_chain_private_client";
const CRASH_PATH: &str = "projects/lc/pages/crash-must-not-land.md";
const CLIENT_INPUT_LIMIT: usize = 4096;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientOperation {
    Read,
    Write,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientInput {
    owner_dir: PathBuf,
    ca_der: Vec<u8>,
    port: u16,
    operation: ClientOperation,
}

fn private_client_input() -> ClientInput {
    // Never block on an interactive terminal or an unattended inherited pipe.
    let mut meta: libc::stat = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::fstat(0, &mut meta) }, 0);
    assert_eq!(
        meta.st_mode & libc::S_IFMT,
        libc::S_IFIFO,
        "private pipe required"
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "private client input timed out");
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = unsafe { libc::poll(&mut fd, 1, 100) };
        assert!(polled >= 0, "private client input poll failed");
        if polled == 0 {
            continue;
        }
        assert!(
            fd.revents & (libc::POLLIN | libc::POLLHUP) != 0,
            "invalid private input pipe"
        );
        let mut chunk = [0u8; 512];
        let count = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        assert!(count >= 0, "private input read failed");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count as usize]);
        assert!(
            bytes.len() <= CLIENT_INPUT_LIMIT,
            "private input exceeds limit"
        );
    }
    // Avoid printing serde errors or the private payload on failure.
    serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("invalid private client input"))
}

#[test]
#[ignore = "private stdin-only client helper; invoked only by the controlled parent harness"]
fn native_chain_private_client() {
    let input = private_client_input();
    assert!((3110..=3199).contains(&input.port));
    assert!(!input.ca_der.is_empty() && input.ca_der.len() <= 2048);
    let owner = &input.owner_dir;
    assert!(owner.is_absolute() && owner.file_name().unwrap() == "owner");
    assert!(
        fs::canonicalize(owner).unwrap() == *owner,
        "noncanonical private owner directory"
    );
    let root = owner.parent().unwrap();
    assert!(root.parent() == Some(Path::new("/tmp")));
    assert!(root
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("c1434-"));
    for path in [root, owner.as_path()] {
        let meta = fs::symlink_metadata(path).unwrap();
        assert!(meta.is_dir() && meta.mode() & 0o077 == 0);
        assert_eq!(meta.uid(), unsafe { libc::geteuid() });
    }
    let issuer = format!("https://issuer.board.localhost:{}", input.port);
    assert!(fs::read(owner.join("trusted-issuer")).unwrap() == issuer.as_bytes());
    let target = RemoteTarget { identity: None, org: SLUG.into(),
    org_id: WORKSPACE.into(),
    endpoint: BOARD.into(), };
    let stored = load_credential(owner, &target).unwrap();
    assert!(stored.issuer.as_deref() == Some(issuer.as_str()));
    let _trust = test_fixture::install(Fixture {
        ca_der: input.ca_der,
        loopback: format!("127.0.0.1:{}", input.port).parse().unwrap(),
    });
    let result = match input.operation {
        ClientOperation::Read => call(
            &target,
            owner,
            "read",
            json!({"path":"projects/lc/pages/new.md"}),
        ),
        ClientOperation::Write => call(
            &target,
            owner,
            "write",
            json!({"path":CRASH_PATH,"text":"crashed write must not land"}),
        ),
    };
    // Do not emit credential bodies or request errors to the test log/stdout.
    assert!(result.is_ok(), "private normal client command failed");
    if let ClientOperation::Read = input.operation {
        assert!(result.unwrap()["text"] == "automatic renewal generation 4");
    }
}

struct OwnedClient {
    child: Option<Child>,
}
impl OwnedClient {
    fn spawn(root: &Path, name: &str) -> Self {
        let log = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join(name))
            .unwrap();
        Self {
            child: Some(
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        CLIENT_HELPER,
                        "--ignored",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .expect("spawn owned private test client"),
            ),
        }
    }
    fn send(&mut self, input: &ClientInput) {
        let bytes = serde_json::to_vec(input).unwrap();
        assert!(bytes.len() <= CLIENT_INPUT_LIMIT);
        // Taking/dropping stdin closes the pipe; the child requires EOF.
        self.child
            .as_mut()
            .unwrap()
            .stdin
            .take()
            .unwrap()
            .write_all(&bytes)
            .unwrap();
    }
    fn status(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.as_mut().unwrap().try_wait().unwrap();
        if status.is_some() {
            // try_wait reaped the child. Never signal this PID again.
            self.child = None;
        }
        status
    }
    fn kill_and_reap(&mut self) -> std::process::ExitStatus {
        let child = self.child.as_mut().unwrap();
        child
            .kill()
            .expect("kill only the owned unreaped test client");
        let status = child.wait().expect("reap killed test client");
        self.child = None;
        status
    }
}
impl Drop for OwnedClient {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CrashMarker {
    committed: bool,
    generation: u64,
}
fn wait_crash_commit(driver: &Driver, child: &mut OwnedClient, path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(
            driver.status().is_none(),
            "AOS peer exited during client crash phase"
        );
        assert!(
            child.status().is_none(),
            "client exited before parent could crash it"
        );
        assert!(Instant::now() < deadline, "crash commit marker timed out");
        if let Ok(file) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            let meta = file.metadata().unwrap();
            assert!(meta.is_file() && meta.mode() & 0o077 == 0 && meta.len() <= 128);
            let mut bytes = Vec::new();
            file.take(129).read_to_end(&mut bytes).unwrap();
            let marker: CrashMarker = serde_json::from_slice(&bytes).unwrap();
            assert!(marker.committed && marker.generation == 6);
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn assert_owner_rotated(previous: &Credential, current: &Credential, generation: u64) {
    let old = previous.cli_session.as_ref().unwrap();
    let new = current.cli_session.as_ref().unwrap();
    assert_eq!(new.generation, generation);
    assert_eq!(old.generation + 1, generation);
    assert!(
        current.access_token != previous.access_token && new.refresh_token != old.refresh_token
    );
    assert!(
        current.organization_id == previous.organization_id
            && current.organization_slug == previous.organization_slug
            && current.subject_id == previous.subject_id
            && current.audience == previous.audience
            && current.issuer == previous.issuer
            && current.capabilities == previous.capabilities
            && new.session_id == old.session_id
            && new.device_label == old.device_label
            && new.absolute_expires_at == old.absolute_expires_at
    );
    assert!(
        new.idle_expires_at >= old.idle_expires_at
            && new.idle_expires_at > now().unwrap()
            && new.idle_expires_at <= new.absolute_expires_at
    );
    assert!(
        current.expires_at > now().unwrap()
            && current.expires_at <= now().unwrap() + 300
            && current.expires_at <= new.absolute_expires_at
    );
    assert!(current.pending.is_none());
}

#[test]
#[ignore = "controlled cross-repo runtime; supply CAD1434_AOS_API_DIR and run --ignored under admission"]
fn native_chain_automatic_renewal_and_real_wiki_mutation() {
    // Fail missing configuration before starting any child or isolated runtime.
    assert!(
        std::env::var_os("CAD1434_AOS_API_DIR").is_some(),
        "explicit AOS worktree required"
    );
    let root = tempfile::Builder::new()
        .prefix("c1434-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for dir in ["home", "xdg", "tmp", "locks", "owner", "member"] {
        fs::create_dir(root.path().join(dir)).unwrap();
        fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _environment = Environment::isolate(root.path());
    let port = free_port(&[]);
    let issuer = format!("https://issuer.board.localhost:{port}");
    let rig = Rig::start(root.path(), &issuer);
    private_write(&root.path().join("native-seam"), rig.seam.as_bytes());
    let (ca_der, cert, key) = pki(root.path());
    let _trust = test_fixture::install(Fixture {
        ca_der: ca_der.clone(),
        loopback: format!("127.0.0.1:{port}").parse().unwrap(),
    });
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).unwrap();
    let nonce = URL_SAFE_NO_PAD.encode(random);
    let mut driver = spawn_driver(root.path(), port, &rig, &cert, &key, &nonce);
    let ready = wait_ready(&mut driver, &root.path().join("tokens.json"));
    assert!(ready.issuer == issuer && ready.audience == BOARD);
    assert!(ready.owner["principal"]["subject_id"] == ready.owner_id);
    assert!(ready.member["principal"]["subject_id"] == ready.member_id);
    let target = RemoteTarget { identity: None, org: SLUG.into(),
    org_id: WORKSPACE.into(),
    endpoint: BOARD.into(), };
    let owner_dir = root.path().join("owner");
    let member_dir = root.path().join("member");
    let grant = crate::remote_enrollment::login_grant_for_test(&ready.owner, &issuer).unwrap();
    assert!(grant.organization_id == WORKSPACE && grant.endpoint == BOARD);
    grant.save_credential(&owner_dir).unwrap();
    private_write(&owner_dir.join("trusted-issuer"), issuer.as_bytes());
    let initial = load_credential(&owner_dir, &target).unwrap();
    assert!(initial.subject_id.as_deref() == Some(ready.owner_id.as_str()));
    assert_eq!(initial.cli_session.as_ref().unwrap().generation, 1);

    // A member exchange is a human CLI login. It does not widen scopes.
    // The saved file below is still what load_credential reads for the calls.
    crate::remote_enrollment::login_grant_for_test(&ready.member, &issuer)
        .expect("member workspace role is accepted");
    assert!(ready.member["principal"]["current_role"] == "member");
    assert!(ready.member["organization_id"] == WORKSPACE && ready.member["audience"] == BOARD);
    let member = Credential {
        organization_id: WORKSPACE.into(),
        organization_slug: Some(SLUG.into()),
        audience: BOARD.into(),
        issuer: Some(issuer.clone()),
        subject_id: Some(ready.member_id.clone()),
        capabilities: serde_json::from_value(ready.member["capabilities"].clone()).unwrap(),
        access_token: ready.member["credential"]["access_token"]
            .as_str()
            .unwrap()
            .to_owned(),
        expires_at: ready.member["credential"]["expires_at"].as_u64().unwrap(),
        cli_session: None,
        pending: None,
    };
    save_credential(&member_dir, SLUG, &member).unwrap();
    assert!(load_credential(&member_dir, &target).is_ok());
    ui::prime_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();

    // Real native seed read, new page, multipart binary and state readback.
    let seed_path = "projects/lc/pages/seed.md";
    rig.rpc("wiki_write", json!({"path":seed_path,"text":"native seed"}));
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":seed_path})).unwrap()["text"],
        "native seed"
    );
    let page = "projects/lc/pages/new.md";
    call(
        &target,
        &owner_dir,
        "write",
        json!({"path":page,"text":"before expiry"}),
    )
    .unwrap();
    assert_eq!(
        rig.rpc("wiki_read", json!({"path":page}))["text"],
        "before expiry"
    );
    // No-overwrite upload enumerates an existing destination directory.
    call(
        &target,
        &owner_dir,
        "write",
        json!({"path":"projects/lc/blobs/seed.md","text":"upload directory fixture"}),
    )
    .unwrap();
    let bytes = b"\x00\x01native chain binary\xff";
    let blob_path = "projects/lc/blobs/new.bin";
    upload_file_in(
        &owner_dir,
        &target,
        blob_path,
        "new.bin",
        bytes,
        false,
        60,
        |_| {},
    )
    .unwrap();
    let blob = rig.rpc("wiki_read", json!({"path":blob_path}));
    assert_eq!(blob["kind"], "blob");
    let vault = crate::wiki::vault_dir(&Pm::at(&rig.pm).unwrap()).unwrap();
    assert_eq!(
        fs::read(crate::wiki::blobs_dir(&vault).join(blob["sha256"].as_str().unwrap())).unwrap(),
        bytes
    );
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":blob_path})).unwrap()["size"],
        bytes.len()
    );

    // Invalid controls must not mutate: the valid expiry still observes live
    // generation 1 and zero renewals; later count/rotation assertions bind it.
    assert_eq!(control(port, "wrong-nonce", "expire_owner_access"), 403);
    assert_eq!(control(port, &nonce, "unknown-op"), 400);
    for generation in 2..=4 {
        let previous = load_credential(&owner_dir, &target).unwrap();
        assert_eq!(
            previous.cli_session.as_ref().unwrap().generation,
            generation - 1
        );
        assert_eq!(control(port, &nonce, "expire_owner_access"), 200);
        expire_client(&owner_dir, &target);
        let before = rig.snapshot();
        let (stale_status, _, _) = post(
            BOARD,
            "/__platform/wiki/authorize",
            &previous.access_token,
            &json!({"version":WIKI_VERSION,"organization_id":WORKSPACE,"audience":BOARD,
                "requested_scopes":["wiki.read"],"ttl_seconds":MAX_TTL}),
            AUTH_TIMEOUT,
        )
        .unwrap();
        assert_eq!(stale_status, 401);
        assert_eq!(rig.snapshot(), before);
        // Normal CLI shared path, NOT an explicit renewal request. Distinct
        // text and binary content prove each generation made a new mutation.
        let text = format!("automatic renewal generation {generation}");
        call(
            &target,
            &owner_dir,
            "write",
            json!({"path":page,"text":text}),
        )
        .unwrap();
        let renewed = load_credential(&owner_dir, &target).unwrap();
        let renewed_session = renewed.cli_session.as_ref().unwrap();
        let initial_session = initial.cli_session.as_ref().unwrap();
        assert_eq!(renewed_session.generation, generation);
        assert!(renewed.access_token != previous.access_token);
        assert!(
            renewed_session.refresh_token != previous.cli_session.as_ref().unwrap().refresh_token
        );
        assert!(
            renewed.organization_id == initial.organization_id
                && renewed.organization_slug == initial.organization_slug
                && renewed.subject_id == initial.subject_id
                && renewed.audience == initial.audience
                && renewed.issuer == initial.issuer
                && renewed.capabilities == initial.capabilities
                && renewed_session.session_id == initial_session.session_id
                && renewed_session.device_label == initial_session.device_label
                && renewed_session.absolute_expires_at == initial_session.absolute_expires_at
        );
        assert!(renewed.pending.is_none());
        assert_eq!(
            call(&target, &owner_dir, "read", json!({"path":page})).unwrap()["text"],
            text
        );
        assert_eq!(rig.rpc("wiki_read", json!({"path":page}))["text"], text);
        let filename = format!("renewed-{generation}.bin");
        let renewed_blob_path = format!("projects/lc/blobs/{filename}");
        let renewed_bytes = [0, generation as u8, 0xff];
        upload_file_in(
            &owner_dir,
            &target,
            &renewed_blob_path,
            &filename,
            &renewed_bytes,
            false,
            60,
            |_| {},
        )
        .unwrap();
        let renewed_blob = rig.rpc("wiki_read", json!({"path":renewed_blob_path}));
        assert_eq!(renewed_blob["kind"], "blob");
        assert_eq!(
            fs::read(crate::wiki::blobs_dir(&vault).join(renewed_blob["sha256"].as_str().unwrap()))
                .unwrap(),
            renewed_bytes
        );
        assert_eq!(
            call(
                &target,
                &owner_dir,
                "read",
                json!({"path":renewed_blob_path})
            )
            .unwrap()["size"],
            renewed_bytes.len()
        );
        assert_eq!(control(port, &nonce, "assert_owner_renewed"), 200);
    }

    // Actual issuer commit followed by a lost wire reply, NOT a client
    // process crash. The next normal command must resume the saved operation,
    // never replay the uncertain original user write.
    let before_drop = load_credential(&owner_dir, &target).unwrap();
    let before_drop_session = before_drop.cli_session.as_ref().unwrap();
    assert_eq!(before_drop_session.generation, 4);
    assert!(before_drop.pending.is_none());
    assert_eq!(control(port, &nonce, "arm_owner_commit_drop"), 200);
    expire_client(&owner_dir, &target);
    let failed_path = "projects/lc/pages/drop-must-not-land.md";
    assert!(!vault.join(failed_path).exists());
    assert!(!vault.join(format!("{failed_path}.blob")).exists());
    let before = rig.snapshot();
    let error = call(
        &target,
        &owner_dir,
        "write",
        json!({"path":failed_path,"text":"lost reply must not land"}),
    )
    .expect_err("dropped renewal reply unexpectedly completed the write");
    assert!(
        error
            .to_string()
            .contains("Hosted CLI session request failed"),
        "drop did not surface the actual issuer wire failure"
    );
    assert!(
        !error.to_string().contains("cadence login"),
        "wire failure recommended a new login"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "failed renewal mutated native state"
    );
    assert!(!vault.join(failed_path).exists());
    assert!(!vault.join(format!("{failed_path}.blob")).exists());
    let failed = load_credential(&owner_dir, &target).unwrap();
    let failed_session = failed.cli_session.as_ref().unwrap();
    assert!(failed.access_token == before_drop.access_token);
    assert!(
        serde_json::to_vec(failed_session).unwrap()
            == serde_json::to_vec(before_drop_session).unwrap()
    );
    assert_eq!(failed_session.generation, 4);
    let pending = failed
        .pending
        .as_ref()
        .expect("wire failure lost pending renewal");
    assert_eq!(pending.expected_generation, 4);
    assert!(valid_operation_id(&pending.operation_id));
    assert_eq!(control(port, &nonce, "assert_owner_commit_dropped"), 200);

    // No explicit/manual renewer: the normal read retries the SAME pending
    // operation with the old refresh bearer and receives the real saved reply.
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal generation 4"
    );
    let recovered = load_credential(&owner_dir, &target).unwrap();
    let recovered_session = recovered.cli_session.as_ref().unwrap();
    assert_eq!(recovered_session.generation, 5);
    assert!(recovered.access_token != before_drop.access_token);
    assert!(recovered_session.refresh_token != before_drop_session.refresh_token);
    assert!(
        recovered.organization_id == before_drop.organization_id
            && recovered.organization_slug == before_drop.organization_slug
            && recovered.subject_id == before_drop.subject_id
            && recovered.audience == before_drop.audience
            && recovered.issuer == before_drop.issuer
            && recovered.capabilities == before_drop.capabilities
            && recovered_session.session_id == before_drop_session.session_id
            && recovered_session.device_label == before_drop_session.device_label
            && recovered_session.absolute_expires_at == before_drop_session.absolute_expires_at
    );
    assert!(recovered.pending.is_none());
    assert_eq!(control(port, &nonce, "assert_owner_recovered"), 200);
    assert_eq!(
        rig.snapshot(),
        before,
        "recovery replayed the failed user write"
    );
    assert!(!vault.join(failed_path).exists());
    assert!(!vault.join(format!("{failed_path}.blob")).exists());

    // Fresh post-recovery operations, on different paths/content. Leave the
    // shared gen4 page intact for the original member read/refusal coverage.
    let recovered_page = "projects/lc/pages/recovered-5.md";
    let recovered_text = "fresh write after pending recovery generation 5";
    assert!(!vault.join(recovered_page).exists());
    call(
        &target,
        &owner_dir,
        "write",
        json!({"path":recovered_page,"text":recovered_text}),
    )
    .unwrap();
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":recovered_page})).unwrap()["text"],
        recovered_text
    );
    assert_eq!(
        rig.rpc("wiki_read", json!({"path":recovered_page}))["text"],
        recovered_text
    );
    let recovered_blob_path = "projects/lc/blobs/recovered-5.bin";
    let recovered_bytes = b"\x00fresh pending recovery binary\x05\xff";
    assert!(!vault.join(recovered_blob_path).exists());
    assert!(!vault.join(format!("{recovered_blob_path}.blob")).exists());
    upload_file_in(
        &owner_dir,
        &target,
        recovered_blob_path,
        "recovered-5.bin",
        recovered_bytes,
        false,
        60,
        |_| {},
    )
    .unwrap();
    let recovered_blob = rig.rpc("wiki_read", json!({"path":recovered_blob_path}));
    assert_eq!(recovered_blob["kind"], "blob");
    assert_eq!(
        fs::read(crate::wiki::blobs_dir(&vault).join(recovered_blob["sha256"].as_str().unwrap()))
            .unwrap(),
        recovered_bytes
    );
    assert_eq!(
        call(
            &target,
            &owner_dir,
            "read",
            json!({"path":recovered_blob_path})
        )
        .unwrap()["size"],
        recovered_bytes.len()
    );

    // Actual test CLIENT PROCESS death: hold the real committed issuer reply
    // before any headers, kill/reap only our child, then resume its saved file.
    // This is not evidence about an installed CLI binary or a wire-only error.
    let before_crash = load_credential(&owner_dir, &target).unwrap();
    assert_eq!(before_crash.cli_session.as_ref().unwrap().generation, 5);
    assert!(before_crash.pending.is_none());
    assert_eq!(control(port, &nonce, "arm_owner_commit_crash"), 200);
    expire_client(&owner_dir, &target);
    let before_crash_expired = load_credential(&owner_dir, &target).unwrap();
    let before = rig.snapshot();
    assert!(!vault.join(CRASH_PATH).exists());
    assert!(!vault.join(format!("{CRASH_PATH}.blob")).exists());
    let mut crashed_client = OwnedClient::spawn(root.path(), "crash-client.log");
    crashed_client.send(&ClientInput {
        owner_dir: owner_dir.clone(),
        ca_der: ca_der.clone(),
        port,
        operation: ClientOperation::Write,
    });
    wait_crash_commit(
        &driver,
        &mut crashed_client,
        &root.path().join("tokens.json.crash-committed"),
    );
    assert_eq!(control(port, &nonce, "assert_owner_crash_committed"), 200);
    assert!(
        crashed_client.status().is_none(),
        "held client already exited"
    );
    let killed = crashed_client.kill_and_reap();
    assert!(!killed.success());
    assert_eq!(killed.signal(), Some(libc::SIGKILL));
    let crashed = load_credential(&owner_dir, &target).unwrap();
    assert!(crashed.access_token == before_crash_expired.access_token);
    assert_eq!(crashed.expires_at, before_crash_expired.expires_at);
    assert!(
        serde_json::to_vec(&crashed.cli_session).unwrap()
            == serde_json::to_vec(&before_crash_expired.cli_session).unwrap()
    );
    let pending = crashed
        .pending
        .as_ref()
        .expect("actual process death lost pending renewal");
    assert_eq!(pending.expected_generation, 5);
    assert!(valid_operation_id(&pending.operation_id));
    assert_eq!(
        rig.snapshot(),
        before,
        "crashed client mutated native state"
    );
    assert_eq!(control(port, &nonce, "release_owner_crash_reply"), 200);
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal generation 4"
    );
    let crash_recovered = load_credential(&owner_dir, &target).unwrap();
    assert_owner_rotated(&before_crash, &crash_recovered, 6);
    assert_eq!(control(port, &nonce, "assert_owner_crash_recovered"), 200);
    assert_eq!(
        rig.snapshot(),
        before,
        "crash recovery replayed the original write"
    );
    assert!(!vault.join(CRASH_PATH).exists());
    assert!(!vault.join(format!("{CRASH_PATH}.blob")).exists());

    // Both real test clients run the normal READ against the same owner file.
    // Only the real production renewal lock/recheck coordinates them.
    assert_eq!(control(port, &nonce, "expire_owner_concurrent_access"), 200);
    expire_client(&owner_dir, &target);
    let before = rig.snapshot();
    let mut clients = [
        OwnedClient::spawn(root.path(), "concurrent-1.log"),
        OwnedClient::spawn(root.path(), "concurrent-2.log"),
    ];
    let read_input = ClientInput {
        owner_dir: owner_dir.clone(),
        ca_der: ca_der.clone(),
        port,
        operation: ClientOperation::Read,
    };
    for child in &mut clients {
        assert!(child.status().is_none());
    }
    for child in &mut clients {
        child.send(&read_input);
    }
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut reaped = [false; 2];
    while !reaped.iter().all(|done| *done) {
        assert!(
            driver.status().is_none(),
            "AOS peer exited during concurrent clients"
        );
        assert!(
            Instant::now() < deadline,
            "concurrent normal clients timed out"
        );
        for (index, child) in clients.iter_mut().enumerate() {
            if !reaped[index] {
                if let Some(status) = child.status() {
                    assert!(
                        status.success(),
                        "concurrent normal client failed (private log)"
                    );
                    reaped[index] = true;
                }
            }
        }
        if !reaped.iter().all(|done| *done) {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let concurrent = load_credential(&owner_dir, &target).unwrap();
    assert_owner_rotated(&crash_recovered, &concurrent, 7);
    assert_eq!(
        rig.snapshot(),
        before,
        "concurrent reads mutated native state"
    );
    assert_eq!(
        control(port, &nonce, "assert_owner_concurrent_renewed"),
        200
    );

    // Finish legacy member operations/denials before its nonrenewable access
    // naturally expires during the owner idle gap.
    assert_eq!(
        call(&target, &member_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal generation 4"
    );
    let own = format!("users/{}/notes.md", ready.member_id);
    call(
        &target,
        &member_dir,
        "write",
        json!({"path":own,"text":"member own"}),
    )
    .unwrap();
    assert_eq!(
        rig.rpc("wiki_read", json!({"path":own}))["text"],
        "member own"
    );
    for (dir, path) in [
        (&member_dir, page),
        (&member_dir, "pms/forbidden.md"),
        (&owner_dir, "pms/forbidden.md"),
    ] {
        let before = rig.snapshot();
        let result = call(
            &target,
            dir,
            "write",
            json!({"path":path,"text":"must not land"}),
        );
        let error = result.expect_err("native forbidden path accepted");
        assert!(
            error.to_string().contains("insufficient_scope"),
            "denial did not come from the actual native grant gate"
        );
        assert!(
            !error.to_string().contains("cadence login"),
            "policy denial suggested re-login"
        );
        assert!(
            error.to_string().contains("a new login does not grant it"),
            "policy denial did not explain that re-login cannot grant authorization"
        );
        assert_eq!(rig.snapshot(), before, "denied write mutated native state");
    }
    for (dir, path) in [
        (&member_dir, "projects/lc/blobs/denied-project.bin"),
        (&member_dir, "pms/denied-member.bin"),
        (&owner_dir, "pms/denied-owner.bin"),
    ] {
        // These fresh private-fixture destinations do not exist. Overwrite is
        // used ONLY for denial so advisory parent listing cannot short-circuit
        // the actual HTTPS -> multipart -> native upload authorization gate.
        assert!(!vault.join(path).exists());
        assert!(!vault.join(format!("{path}.blob")).exists());
        let before = rig.snapshot();
        let error = upload_file_in(
            dir,
            &target,
            path,
            "denied.bin",
            b"\x00must not land\xff",
            true,
            60,
            |_| {},
        )
        .expect_err("native forbidden upload accepted");
        assert!(
            error.to_string().contains("authorization")
                && error.to_string().contains("insufficient_scope"),
            "upload denial did not come from the actual native grant gate"
        );
        assert!(
            !error.to_string().contains("cadence login"),
            "upload policy denial suggested re-login"
        );
        assert!(
            error.to_string().contains("a new login does not grant it"),
            "upload policy denial did not explain that re-login cannot grant authorization"
        );
        assert_eq!(rig.snapshot(), before, "denied upload mutated native state");
        assert!(!vault.join(path).exists());
        assert!(!vault.join(format!("{path}.blob")).exists());
    }
    // Natural idle gap, not SQL/manual expiry and not a fake clock. No call
    // touches the owner session until its actual 300s access lifetime elapses.
    let idle = load_credential(&owner_dir, &target).unwrap();
    assert!(idle.pending.is_none());
    assert_eq!(idle.cli_session.as_ref().unwrap().generation, 7);
    let sleep_seconds = idle
        .expires_at
        .checked_add(1)
        .unwrap()
        .saturating_sub(now().unwrap());
    assert!(sleep_seconds > 0 && sleep_seconds <= 310);
    let before = rig.snapshot();
    std::thread::sleep(Duration::from_secs(sleep_seconds));
    assert!(
        driver.status().is_none(),
        "AOS peer exited during natural idle gap"
    );
    assert!(now().unwrap() > idle.expires_at);
    assert!(idle.cli_session.as_ref().unwrap().idle_expires_at > now().unwrap());
    assert!(idle.cli_session.as_ref().unwrap().absolute_expires_at > now().unwrap());
    // Re-prime ONLY the existing controller-owned JWKS cache over verified TLS.
    // Native handlers still do not prove a fresh handler-owned TLS JWKS fetch.
    ui::prime_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    assert_eq!(control(port, &nonce, "assert_owner_idle_gap"), 200);
    let (stale_status, _, _) = post(
        BOARD,
        "/__platform/wiki/authorize",
        &idle.access_token,
        &json!({"version":WIKI_VERSION,"organization_id":WORKSPACE,"audience":BOARD,
            "requested_scopes":["wiki.read"],"ttl_seconds":MAX_TTL}),
        AUTH_TIMEOUT,
    )
    .unwrap();
    assert_eq!(stale_status, 401);
    assert_eq!(
        rig.snapshot(),
        before,
        "expired idle bearer mutated native state"
    );
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal generation 4"
    );
    let idle_renewed = load_credential(&owner_dir, &target).unwrap();
    assert_owner_rotated(&idle, &idle_renewed, 8);
    assert_eq!(
        rig.snapshot(),
        before,
        "idle renewal/read mutated native state"
    );
    let idle_page = "projects/lc/pages/idle-8.md";
    let idle_text = "fresh owner write after natural idle expiry generation 8";
    assert!(!vault.join(idle_page).exists());
    call(
        &target,
        &owner_dir,
        "write",
        json!({"path":idle_page,"text":idle_text}),
    )
    .unwrap();
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":idle_page})).unwrap()["text"],
        idle_text
    );
    assert_eq!(
        rig.rpc("wiki_read", json!({"path":idle_page}))["text"],
        idle_text
    );
    let idle_blob_path = "projects/lc/blobs/idle-8.bin";
    let idle_bytes = b"\x00fresh natural idle binary\x08\xff";
    assert!(!vault.join(idle_blob_path).exists());
    assert!(!vault.join(format!("{idle_blob_path}.blob")).exists());
    upload_file_in(
        &owner_dir,
        &target,
        idle_blob_path,
        "idle-8.bin",
        idle_bytes,
        false,
        60,
        |_| {},
    )
    .unwrap();
    let idle_blob = rig.rpc("wiki_read", json!({"path":idle_blob_path}));
    assert_eq!(idle_blob["kind"], "blob");
    assert_eq!(
        fs::read(crate::wiki::blobs_dir(&vault).join(idle_blob["sha256"].as_str().unwrap()))
            .unwrap(),
        idle_bytes
    );
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":idle_blob_path})).unwrap()["size"],
        idle_bytes.len()
    );
    assert!(load_credential(&owner_dir, &target)
        .unwrap()
        .pending
        .is_none());
    assert_eq!(control(port, &nonce, "assert_owner_idle_renewed"), 200);

    // An allowed read root is not a write grant: member failed project write
    // above, and owner pms remains forbidden despite its operator actor.
    assert_eq!(control(port, &nonce, "revoke_owner_session"), 200);
    expire_client(&owner_dir, &target);
    let before = rig.snapshot();
    let error = call(&target, &owner_dir, "read", json!({"path":page})).unwrap_err();
    assert!(
        error.to_string().contains("login"),
        "renewal refusal lacks re-login remedy"
    );
    assert!(
        error.to_string().contains("issuer request "),
        "actual issuer correlation headers did not reach the CLI"
    );
    assert_eq!(rig.snapshot(), before);
    assert!(
        owner_dir.join("cli-lc.json").is_file(),
        "refusal deleted saved credential"
    );
    assert_eq!(control(port, &nonce, "finish"), 200);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = driver.status() {
            assert!(status, "controlled AOS peer failed (private driver.log)");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "controlled AOS peer did not finish"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(feature = "cli-runtime-test")]
include!("cad1568_native_messages_acceptance.rs");
