//! CAD-913 independent refusal acceptance, authored from the ticket.
//! The implementer must not edit or weaken this check.
//!
//! Exercises the real public-host HTTP dispatcher, envelope signature/pin
//! verifier, wiki upload handler, and daemon wiki_write/wiki_put_blob paths.
//! The native board-to-daemon connection alone uses the supported test seam;
//! signed actor, scopes, prefix grants and signature are real. All state and
//! keys are synthetic and isolated under /tmp. No live hosts or credentials.
//!
//! Run under an admitted test slot:
//! cadence build-slot run test --org local -- env CARGO_BUILD_JOBS=4 cargo test
//!   --features test-seam --test cad913_wiki_envelope_upload
//!   -- --nocapture --test-threads 1
#![cfg(feature = "test-seam")]

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, ui};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

const HOST: &str = "cad913.cadencecloud.test";
const ORG: &str = "ws_cad913_fixture";
const KID: &str = "cad913-independent-fixture";
const PREFIX: &str = "global/cad913/";

type SnapshotFiles = Vec<(String, Vec<u8>)>;
type Snapshot = (String, SnapshotFiles, SnapshotFiles);

fn port() -> u16 {
    (3110..=3199)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no isolated port in 3110..3199")
}

struct Rig {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<cadence_agent::Result<()>>>,
    port: u16,
    issuer: String,
    key: Ed25519KeyPair,
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
    fn start() -> Self {
        Self::start_with_read_only(false)
    }

    fn start_with_read_only(read_only: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("c913-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "tmp", "locks", "s"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        std::env::set_var("HOME", root.path().join("home"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("xdg"));
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("home/config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("home/data"));
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        let pm_dir = root.path().join("pm");
        let mut pm = Pm::init(&pm_dir).unwrap();
        pm.config.wiki.max_upload_bytes = 128;
        std::fs::write(
            pm_dir.join("pm.yaml"),
            serde_yaml::to_string(&pm.config).unwrap(),
        )
        .unwrap();
        let state = root.path().join("s");
        let stop = Arc::new(AtomicBool::new(false));
        let mut rig = Self {
            root,
            stop,
            threads: Vec::new(),
            port: 0,
            issuer: String::new(),
            key: Ed25519KeyPair::from_seed_unchecked(&[193; 32]).unwrap(),
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
        rig.threads
            .push(thread::spawn(move || daemon::serve_with(&run_state, opts)));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                if let Some(token) = Seam::token_at(&state) {
                    rig.seam = token;
                    break;
                }
            }
            if rig.threads[0].is_finished() {
                panic!(
                    "isolated daemon startup failed: {:?}",
                    rig.threads.remove(0).join()
                );
            }
            assert!(Instant::now() < until, "isolated daemon startup timed out");
            thread::sleep(Duration::from_millis(50));
        }
        let jwks = Server::http(("127.0.0.1", port())).unwrap();
        rig.issuer = format!("http://{}", jwks.server_addr());
        let public_key = URL_SAFE_NO_PAD.encode(rig.key.public_key().as_ref());
        let jwks_stop = rig.stop.clone();
        rig.threads.push(thread::spawn(move || {
            while !jwks_stop.load(SeqCst) {
                if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                    assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
                    request
                        .respond(
                            Response::from_string(
                                json!({"keys":[{
                                    "kty":"OKP", "crv":"Ed25519", "kid":KID, "x":public_key
                                }]})
                                .to_string(),
                            )
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json").unwrap(),
                            ),
                        )
                        .unwrap();
                }
            }
            Ok(())
        }));
        rig.port = port();
        let (ready, wait) = std::sync::mpsc::channel();
        let opts = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: rig.port,
            allow_hosts: vec![HOST.into()],
            board_public_only: true,
            read_only,
            public: Some(ui::PublicBoard {
                host: HOST.into(),
                issuer: rig.issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{}/authorize", rig.issuer),
            }),
            test_seam: true,
            stop: Some(rig.stop.clone()),
            startup: Some(ready),
            gh: Some("/bin/false".into()),
            ..Default::default()
        };
        let state_pm = pm_dir.clone();
        rig.threads
            .push(thread::spawn(move || ui::serve(&state, &state_pm, &opts)));
        wait.recv_timeout(Duration::from_secs(30))
            .expect("board startup disconnected")
            .unwrap();
        rig
    }

    fn claims(&self, jti: &str) -> Value {
        let now = cadence_agent::issue::time::now_epoch();
        json!({
            "iss":self.issuer, "aud":format!("https://{HOST}"), "sub":"usr_cad913",
            "actor":"operator", "principal_kind":"user", "role_at_issue":"owner",
            "organization_id":ORG, "scope":["wiki.read","wiki.write"],
            "wiki_prefixes":[PREFIX], "prefix_grants":[
                {"scope":"wiki.read","prefixes":[PREFIX]},
                {"scope":"wiki.write","prefixes":[PREFIX]}
            ], "credential_id":"hcp_cad913", "iat":now, "exp":now+120, "jti":jti
        })
    }

    fn sign(&self, claims: &Value) -> String {
        let h = URL_SAFE_NO_PAD
            .encode(json!({"alg":"EdDSA","typ":"agenticos-wiki-actor/1","kid":KID}).to_string());
        let p = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing = format!("{h}.{p}");
        format!(
            "{signing}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signing.as_bytes()).as_ref())
        )
    }

    fn multipart(
        &self,
        path: &str,
        bearer: Option<&str>,
        parts: &[(&str, &str, &[u8])],
    ) -> (u16, Value) {
        let boundary = "cad913-boundary";
        let mut body = Vec::new();
        for (name, filename, bytes) in parts {
            write!(&mut body, "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n").unwrap();
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        write!(&mut body, "--{boundary}--\r\n").unwrap();
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(socket, "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len()).unwrap();
        write!(
            socket,
            "X-Cadence-Test-As: operator\r\nX-Cadence-Test-Token: {}\r\n",
            self.seam
        )
        .unwrap();
        if let Some(token) = bearer {
            write!(socket, "Authorization: Bearer wikienv_{token}\r\n").unwrap();
        }
        socket.write_all(b"\r\n").unwrap();
        socket.write_all(&body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let at = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let status = String::from_utf8_lossy(&response[..at])
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let value = serde_json::from_slice(&response[at + 4..])
            .unwrap_or_else(|_| json!({"raw":String::from_utf8_lossy(&response[at+4..])}));
        (status, value)
    }

    // Headers declare a body but withhold it; a timely refusal proves auth
    // happens before the multipart reader waits for that body.
    fn refusal_before_body(&self, path: &str, bearer: &str) -> Option<u16> {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(socket, "POST {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: multipart/form-data; boundary=cad913-boundary\r\nContent-Length: 1048576\r\nConnection: close\r\nX-Cadence-Test-As: operator\r\nX-Cadence-Test-Token: {}\r\nAuthorization: Bearer wikienv_{bearer}\r\n\r\n", self.seam).unwrap();
        let mut response = Vec::new();
        if socket.read_to_end(&mut response).is_err() {
            return None;
        }
        let at = response.windows(4).position(|w| w == b"\r\n\r\n")?;
        String::from_utf8_lossy(&response[..at])
            .lines()
            .next()?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()
    }

    fn state(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }

    // Compare tracker HEAD, wiki bytes, and every staged upload byte. This
    // catches both a daemon mutation and pre-auth/pre-cap staging side effects.
    fn snapshot(&self) -> Snapshot {
        fn walk(dir: &std::path::Path, out: &mut SnapshotFiles) {
            if !dir.exists() {
                return;
            }
            for entry in std::fs::read_dir(dir).unwrap() {
                let p = entry.unwrap().path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push((p.to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
                }
            }
        }
        let head = cadence_agent::proc::run_bounded(
            std::process::Command::new("git").args([
                "-C",
                self.pm().to_str().unwrap(),
                "rev-parse",
                "HEAD",
            ]),
            Duration::from_secs(15),
        )
        .unwrap();
        assert!(head.status.success());
        let mut wiki = Vec::new();
        let pm = Pm::at(&self.pm()).unwrap();
        walk(&cadence_agent::wiki::vault_dir(&pm).unwrap(), &mut wiki);
        let mut staged = Vec::new();
        walk(&self.state().join("wiki-uploads"), &mut staged);
        wiki.sort();
        staged.sort();
        (String::from_utf8(head.stdout).unwrap(), wiki, staged)
    }
}

fn multipart_file<'a>(
    filename: &'a str,
    bytes: &'a [u8],
) -> [(&'static str, &'a str, &'a [u8]); 1] {
    [("file", filename, bytes)]
}

#[test]
fn wiki_envelope_upload_refuses_before_staging_and_commits_only_verified_targets() {
    let rig = Rig::start();
    let valid = rig.sign(&rig.claims("owner-valid-text"));
    let query = format!("/api/wiki/upload?path={PREFIX}owner.txt");

    // A missing/invalid envelope, expired envelope, and read-only scope must
    // stop before creating the staging directory or touching the wiki store.
    let invalid = "not-a-signed-envelope";
    let before = rig.snapshot();
    let (status, _) = rig.multipart(&query, Some(invalid), &multipart_file("x.txt", b"denied"));
    assert!(
        (400..500).contains(&status) && rig.snapshot() == before,
        "invalid envelope status={status}"
    );
    assert_eq!(
        rig.refusal_before_body(&query, invalid),
        Some(401),
        "invalid envelope must refuse before body read"
    );

    // A well-formed envelope signed by the pinned fixture key is the control;
    // changing its claims without resigning must fail signature verification.
    let signed = rig.sign(&rig.claims("tampered-signature"));
    let mut pieces: Vec<String> = signed.split('.').map(str::to_owned).collect();
    let forged = json!({"iss":rig.issuer,"aud":format!("https://{HOST}"),"sub":"usr_cad913","actor":"operator"});
    pieces[1] = URL_SAFE_NO_PAD.encode(forged.to_string());
    let before = rig.snapshot();
    let (status, _) = rig.multipart(
        &query,
        Some(&pieces.join(".")),
        &multipart_file("x.txt", b"denied"),
    );
    assert_eq!(status, 401, "tampered signed claims");
    assert_eq!(
        rig.snapshot(),
        before,
        "tampered signature staged or mutated data"
    );

    let mut unsupported_actor = rig.claims("unsupported-actor-upload");
    unsupported_actor["actor"] = json!("agents/agent_fixture");
    unsupported_actor["principal_kind"] = json!("agent");
    unsupported_actor["role_at_issue"] = json!("owner");
    unsupported_actor["credential_id"] = json!("hcc_fixture");
    let before = rig.snapshot();
    let (status, _) = rig.multipart(
        &query,
        Some(&rig.sign(&unsupported_actor)),
        &multipart_file("x.txt", b"denied"),
    );
    assert_eq!(status, 403, "unsupported wiki actor");
    assert_eq!(
        rig.snapshot(),
        before,
        "unsupported actor staged or mutated data"
    );

    let mut expired = rig.claims("expired-upload");
    let now = cadence_agent::issue::time::now_epoch();
    expired["iat"] = json!(now - 180);
    expired["exp"] = json!(now - 60);
    let before = rig.snapshot();
    let (status, _) = rig.multipart(
        &query,
        Some(&rig.sign(&expired)),
        &multipart_file("x.txt", b"denied"),
    );
    assert_eq!(status, 401, "expired envelope");
    assert_eq!(
        rig.snapshot(),
        before,
        "expired envelope staged or mutated data"
    );

    let mut readonly = rig.claims("readonly-upload");
    readonly["scope"] = json!(["wiki.read"]);
    readonly["prefix_grants"] = json!([{"scope":"wiki.read","prefixes":[PREFIX]}]);
    let before = rig.snapshot();
    let (status, _) = rig.multipart(
        &query,
        Some(&rig.sign(&readonly)),
        &multipart_file("x.txt", b"denied"),
    );
    assert_eq!(status, 403, "read-only wiki scope");
    assert_eq!(
        rig.snapshot(),
        before,
        "read-only scope staged or mutated data"
    );

    // The checked query path is authoritative. A multipart path cannot
    // redirect a covered upload outside the signed write prefix.
    let override_path = format!("/api/wiki/upload?path={PREFIX}must-not-land.txt");
    let before = rig.snapshot();
    let parts = [
        ("path", "ignored", b"global/outside/escape.txt".as_slice()),
        ("file", "x.txt", b"escape".as_slice()),
    ];
    let (status, _) = rig.multipart(
        &override_path,
        Some(&rig.sign(&rig.claims("path-override"))),
        &parts,
    );
    assert!(
        (400..500).contains(&status),
        "multipart path override accepted: {status}"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "multipart path override created staging or wiki data"
    );

    // Out-of-prefix query paths and a multiple-file request fail closed.
    let before = rig.snapshot();
    let (status, _) = rig.multipart(
        "/api/wiki/upload?path=global/outside/escape.txt",
        Some(&rig.sign(&rig.claims("outside-upload"))),
        &multipart_file("x.txt", b"escape"),
    );
    assert_eq!(status, 403, "out-of-prefix target");
    assert_eq!(
        rig.snapshot(),
        before,
        "out-of-prefix upload staged or mutated data"
    );
    let before = rig.snapshot();
    let (status, _) = rig.multipart(&query, Some(&rig.sign(&rig.claims("missing-file"))), &[]);
    assert!(
        (400..500).contains(&status),
        "multipart without a file accepted: {status}"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "malformed multipart staged or mutated data"
    );
    let before = rig.snapshot();
    let files = [
        ("file", "a.txt", b"first".as_slice()),
        ("file", "b.txt", b"second".as_slice()),
    ];
    let (status, _) = rig.multipart(
        &query,
        Some(&rig.sign(&rig.claims("multiple-files"))),
        &files,
    );
    assert!(
        (400..500).contains(&status),
        "multiple file parts accepted: {status}"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "multiple file parts staged or mutated data"
    );

    // Configured size-cap refusal must not leave staged bytes or a wiki
    // mutation. The configured cap is intentionally tiny in this fixture.
    let before = rig.snapshot();
    let too_large = vec![b'x'; 129];
    let (status, _) = rig.multipart(
        &query,
        Some(&rig.sign(&rig.claims("oversize-upload"))),
        &multipart_file("large.bin", &too_large),
    );
    assert!(
        (400..500).contains(&status),
        "over-cap file accepted: {status}"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "over-cap file staged or mutated data"
    );

    // A write prefix grants descendants, not the directory itself. A PDF at
    // the exact grant path could otherwise create its extracted Markdown
    // sibling outside the signed prefix. Keep this check before any successful
    // upload and prove the fixture directory does not already exist.
    let vault = cadence_agent::wiki::vault_dir(&Pm::at(&rig.pm()).unwrap()).unwrap();
    assert!(
        !vault.join(PREFIX).exists(),
        "directory-target fixture must start absent"
    );
    let directory_query = format!("/api/wiki/upload?path={}", PREFIX.trim_end_matches('/'));
    let directory_token = rig.sign(&rig.claims("directory-prefix-upload"));
    let before = rig.snapshot();
    assert_eq!(
        rig.refusal_before_body(&directory_query, &directory_token),
        Some(403),
        "exact granted-prefix directory target must refuse before body read"
    );
    assert_eq!(
        rig.snapshot(),
        before,
        "pre-body directory-target refusal staged or mutated data"
    );
    let pdf_token = rig.sign(&rig.claims("directory-prefix-upload-pdf"));
    let (status, body) = rig.multipart(
        &directory_query,
        Some(&pdf_token),
        &multipart_file("directory.pdf", b"%PDF-1.7\nsynthetic fixture"),
    );
    assert_eq!(status, 403, "PDF upload to exact prefix directory: {body}");
    assert_eq!(
        rig.snapshot(),
        before,
        "directory-target PDF staged or mutated data"
    );

    // Positive controls: text and binary go through the real existing daemon
    // mutation routes with owner and member attribution respectively.
    let (status, body) = rig.multipart(
        &query,
        Some(&valid),
        &multipart_file("owner.txt", b"owner upload text"),
    );
    assert_eq!(status, 200, "owner text upload: {body}");
    assert!(
        body["path"]
            .as_str()
            .unwrap_or_default()
            .contains("owner.txt"),
        "text response: {body}"
    );
    let text = scoped(Asserted::Operator, || {
        client::rpc(
            &rig.state(),
            "wiki_read",
            json!({"path":format!("{PREFIX}owner.txt")}),
        )
    })
    .unwrap();
    assert_eq!(text["text"], "owner upload text");

    let mut member = rig.claims("member-valid-binary");
    member["actor"] = json!("users/member_fixture");
    member["role_at_issue"] = json!("member");
    member["wiki_prefixes"] = json!(["users/member_fixture/"]);
    member["prefix_grants"] = json!([
        {"scope":"wiki.read","prefixes":["users/member_fixture/"]},
        {"scope":"wiki.write","prefixes":["users/member_fixture/"]}
    ]);
    let bytes = b"\x00\x01cad913 binary fixture\xff";
    let member_path = "/api/wiki/upload?path=users/member_fixture/fixture.bin";
    let (status, body) = rig.multipart(
        member_path,
        Some(&rig.sign(&member)),
        &multipart_file("fixture.bin", bytes),
    );
    assert_eq!(status, 200, "member binary upload: {body}");
    assert_eq!(body["path"], "users/member_fixture/fixture.bin");
    let page = scoped(Asserted::Operator, || {
        client::rpc(
            &rig.state(),
            "wiki_read",
            json!({"path":"users/member_fixture/fixture.bin"}),
        )
    })
    .unwrap();
    assert_eq!(page["kind"], "blob");
    assert_eq!(page["size"], bytes.len());
    let sha = page["sha256"].as_str().expect("blob digest");
    let vault = cadence_agent::wiki::vault_dir(&Pm::at(&rig.pm()).unwrap()).unwrap();
    let blob = cadence_agent::wiki::blobs_dir(&vault).join(sha);
    assert_eq!(std::fs::read(blob).unwrap(), bytes);

    // The board's independent --read-only mode is also checked before the
    // request body is consumed or staged (the file remains in the client).
    drop(rig);
    let readonly_rig = Rig::start_with_read_only(true);
    let before = readonly_rig.snapshot();
    let token = readonly_rig.sign(&readonly_rig.claims("board-read-only-upload"));
    let (status, _) = readonly_rig.multipart(
        &format!("/api/wiki/upload?path={PREFIX}readonly.txt"),
        Some(&token),
        &multipart_file("readonly.txt", b"must not stage"),
    );
    assert_eq!(status, 403, "read-only board upload");
    assert_eq!(
        readonly_rig.snapshot(),
        before,
        "read-only board staged or mutated data"
    );
}

impl Rig {
    /// A raw HTTP request to the board with the envelope bearer and the seam
    /// headers. Returns `(status, parsed JSON body)`. The body is bounded.
    fn raw(&self, method: &str, path: &str, bearer: &str, body: Option<&Value>) -> (u16, Value) {
        let mut socket = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        write!(
            socket,
            "{method} {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\nX-Cadence-Test-As: operator\r\n\
             X-Cadence-Test-Token: {}\r\nAuthorization: Bearer wikienv_{bearer}\r\n\r\n{payload}",
            payload.len(),
            self.seam
        )
        .unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let at = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let status = String::from_utf8_lossy(&response[..at])
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let value = serde_json::from_slice(&response[at + 4..])
            .unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(&response[at + 4..])}));
        (status, value)
    }
}

/// CAD-1434 / AOS-243 native write/read: the real board `PUT`/`GET
/// /api/wiki/file` route, the real envelope verifier, the real container
/// prefix gate, and the real daemon `wiki_write`/`wiki_read` RPC. Nothing is
/// recorded or faked. A write outside the signed prefix is refused with no
/// daemon mutation; the signed own-path write succeeds and reads back through
/// the daemon.
#[test]
fn wiki_write_and_read_are_denied_outside_the_signed_prefix_and_allowed_on_it() {
    let rig = Rig::start();
    let token = rig.sign(&rig.claims("native-write-read"));

    // Outside the signed write prefix: refused by the container gate before any
    // daemon mutation. The shared snapshot must not change.
    let before = rig.snapshot();
    let (status, body) = rig.raw(
        "PUT",
        "/api/wiki/file",
        &token,
        Some(&json!({"path": "global/outside/escape.md", "text": "escape"})),
    );
    assert_eq!(status, 403, "outside-prefix write: {body}");
    assert_eq!(body["error"]["code"], "insufficient_scope", "{body}");
    assert_eq!(rig.snapshot(), before, "outside-prefix write mutated data");

    // Inside the signed prefix: the daemon writes it, and the daemon reads it back.
    // An envelope is single-use (its jti is consumed by the first request), so each
    // request mints a fresh envelope, as the real client does per command.
    let own = format!("{PREFIX}native.md");
    let write_token = rig.sign(&rig.claims("native-own-write"));
    let (status, body) = rig.raw(
        "PUT",
        "/api/wiki/file",
        &write_token,
        Some(&json!({"path": own, "text": "native own-path text"})),
    );
    assert_eq!(status, 200, "own-path write: {body}");
    let read_token = rig.sign(&rig.claims("native-own-read"));
    let (status, read) = rig.raw(
        "GET",
        &format!("/api/wiki/file?path={own}"),
        &read_token,
        None,
    );
    assert_eq!(status, 200, "own-path read: {read}");
    assert_eq!(read["text"], "native own-path text", "{read}");
}
