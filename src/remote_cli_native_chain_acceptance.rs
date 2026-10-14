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
use std::os::unix::process::CommandExt;
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
        ca_der,
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
    let target = RemoteTarget {
        org: SLUG.into(),
        org_id: WORKSPACE.into(),
        endpoint: BOARD.into(),
    };
    let owner_dir = root.path().join("owner");
    let member_dir = root.path().join("member");
    let grant = crate::remote_enrollment::login_grant_for_test(&ready.owner, &issuer).unwrap();
    assert!(grant.organization_id == WORKSPACE && grant.endpoint == BOARD);
    grant.save_credential(&owner_dir).unwrap();
    private_write(&owner_dir.join("trusted-issuer"), issuer.as_bytes());
    let initial = load_credential(&owner_dir, &target).unwrap();
    assert!(initial.subject_id.as_deref() == Some(ready.owner_id.as_str()));
    assert_eq!(initial.cli_session.as_ref().unwrap().generation, 1);

    // Do not weaken owner-only device/login consent for member coverage.
    // The actual member exchange is mapped to the existing legacy credential
    // store and then read by the strict production load_credential parser.
    assert!(crate::remote_enrollment::login_grant_for_test(&ready.member, &issuer).is_err());
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
    assert_eq!(control(port, &nonce, "expire_owner_access"), 200);
    expire_client(&owner_dir, &target);
    let before = rig.snapshot();
    let (stale_status, _, _) = post(
        BOARD,
        "/__platform/wiki/authorize",
        &initial.access_token,
        &json!({"version":WIKI_VERSION,"organization_id":WORKSPACE,"audience":BOARD,
            "requested_scopes":["wiki.read"],"ttl_seconds":MAX_TTL}),
        AUTH_TIMEOUT,
    )
    .unwrap();
    assert_eq!(stale_status, 401);
    assert_eq!(rig.snapshot(), before);
    // This is the normal CLI shared path, NOT an explicit renewal request.
    call(
        &target,
        &owner_dir,
        "write",
        json!({"path":page,"text":"automatic renewal"}),
    )
    .unwrap();
    let renewed = load_credential(&owner_dir, &target).unwrap();
    assert_eq!(renewed.cli_session.as_ref().unwrap().generation, 2);
    assert!(renewed.access_token != initial.access_token);
    assert!(
        renewed.cli_session.as_ref().unwrap().refresh_token
            != initial.cli_session.as_ref().unwrap().refresh_token
    );
    assert!(
        renewed.subject_id == initial.subject_id
            && renewed.audience == initial.audience
            && renewed.issuer == initial.issuer
    );
    assert!(renewed.pending.is_none());
    assert_eq!(
        call(&target, &owner_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal"
    );
    assert_eq!(
        rig.rpc("wiki_read", json!({"path":page}))["text"],
        "automatic renewal"
    );
    let renewed_blob_path = "projects/lc/blobs/renewed.bin";
    upload_file_in(
        &owner_dir,
        &target,
        renewed_blob_path,
        "renewed.bin",
        bytes,
        false,
        60,
        |_| {},
    )
    .unwrap();
    let renewed_blob = rig.rpc("wiki_read", json!({"path":renewed_blob_path}));
    assert_eq!(
        fs::read(crate::wiki::blobs_dir(&vault).join(renewed_blob["sha256"].as_str().unwrap()))
            .unwrap(),
        bytes
    );
    assert_eq!(
        call(
            &target,
            &owner_dir,
            "read",
            json!({"path":renewed_blob_path})
        )
        .unwrap()["size"],
        bytes.len()
    );
    assert_eq!(control(port, &nonce, "assert_owner_renewed"), 200);

    assert_eq!(
        call(&target, &member_dir, "read", json!({"path":page})).unwrap()["text"],
        "automatic renewal"
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
