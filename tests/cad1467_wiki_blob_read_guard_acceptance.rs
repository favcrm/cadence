//! CAD-1467: independently authored guard acceptance for remote blob reads.
//! Implementation authors must not edit or weaken this file.
//!
//! Proves the real signed wiki HTTP route and the daemon `wiki_read` RPC,
//! not a mock forwarder. A covered binary slice is the positive control.
//! Out-of-prefix, unauthorized, and non-operator HTTP peers refuse before
//! any original byte (raw or standard base64) is released. A stale pointer
//! revision refuses with no bytes on both transports. Replay, signature,
//! path ACL, and text-range behavior stay as they are.
//!
//! Blob range success (any of `offset` / `bytes` / `rev` present) is JSON:
//! `path`, `kind:"blob"`, `sha256`, `size`, `mime`, `name`, `rev`,
//! `data_base64` (standard base64 with padding of exactly `[start, end)`),
//! `range: {start, end, total}` with `end` exclusive. Same keys as a text
//! range, with `data_base64` instead of `text`. No other keys. No-range
//! reads stay pointer metadata: `path`, `kind`, `sha256`, `size`, `mime`,
//! `name`, `rev` only.
//!
//! Run under an admitted test slot. Preserve `RUSTC_WRAPPER`.
//! cadence build-slot run test --org local -- env CARGO_BUILD_JOBS=4 cargo test
//!   --features test-seam --test cad1467_wiki_blob_read_guard_acceptance
//!   -- --nocapture --test-threads 2
#![cfg(feature = "test-seam")]

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::wiki::NATIVE_TOOL_LINE_BOUND;
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, ui, wiki};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering::SeqCst},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

const HOST: &str = "cad1467.cadencecloud.test";
const ORG: &str = "ws_cad1467_fixture";
const KID: &str = "cad1467-independent-guard";
const COVERED: &str = "global/covered/proof.bin";
const OUTSIDE: &str = "global/outside/secret.bin";
const NOTE: &str = "global/covered/ordinary note.md";
const NOTE_TEXT: &str = "# CAD-1467 text stays text\n";
const START: u64 = 8;
const LEN: u64 = 16;
const READ_PREFIX: &str = "global/covered/";

fn covered_bytes() -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend_from_slice(b"CAD1467 covered blob guard");
    bytes.resize(48, 0xa5);
    bytes
}

fn outside_bytes() -> Vec<u8> {
    let mut bytes = vec![0xfd, 0xfc];
    bytes.extend_from_slice(b"CAD1467 outside blob secret");
    bytes.resize(48, 0x5a);
    bytes
}

fn replaced_bytes() -> Vec<u8> {
    let mut bytes = vec![0xfb, 0xfa];
    bytes.extend_from_slice(b"CAD1467 replaced blob bytes");
    bytes.resize(40, 0x3c);
    bytes
}

fn slice_of(bytes: &[u8]) -> &[u8] {
    let start = START as usize;
    &bytes[start..start + LEN as usize]
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fnv1a(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a:{hash:016x}")
}

fn free_port(mut range: std::ops::Range<u16>) -> u16 {
    range
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("isolated port")
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|window| window == needle)
}

fn leaks(body: &[u8], secrets: &[&[u8]]) -> bool {
    secrets.iter().any(|secret| {
        contains_bytes(body, secret) || contains_bytes(body, STANDARD.encode(secret).as_bytes())
    })
}

fn keys_of(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    keys.sort();
    keys
}

struct Rig {
    root: tempfile::TempDir,
    pm: PathBuf,
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
        let root = tempfile::Builder::new()
            .prefix("c1467-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["home", "xdg", "locks", "s"] {
            std::fs::create_dir(root.path().join(dir)).unwrap();
        }
        std::env::set_var("HOME", root.path().join("home"));
        std::env::set_var("XDG_STATE_HOME", root.path().join("xdg"));
        std::env::set_var("XDG_CONFIG_HOME", root.path().join("home/config"));
        std::env::set_var("XDG_DATA_HOME", root.path().join("home/data"));
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let state = root.path().join("s");
        let stop = Arc::new(AtomicBool::new(false));
        let mut rig = Self {
            root,
            pm: pm.dir.clone(),
            stop,
            threads: Vec::new(),
            port: 0,
            issuer: String::new(),
            key: Ed25519KeyPair::from_seed_unchecked(&[167; 32]).unwrap(),
            seam: String::new(),
        };
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
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
                let outcome = rig.threads.remove(0).join();
                panic!("isolated daemon startup failed: {outcome:?}");
            }
            assert!(Instant::now() < until, "isolated daemon startup timed out");
            thread::sleep(Duration::from_millis(50));
        }
        let jwks = Server::http(("127.0.0.1", free_port(3110..3155))).unwrap();
        rig.issuer = format!("http://{}", jwks.server_addr());
        let public_key = URL_SAFE_NO_PAD.encode(rig.key.public_key().as_ref());
        let jwks_stop = rig.stop.clone();
        rig.threads.push(thread::spawn(move || {
            while !jwks_stop.load(SeqCst) {
                if let Some(request) = jwks.recv_timeout(Duration::from_millis(100)).unwrap() {
                    if request.url().split('?').next()
                        != Some("/.well-known/agenticos-board-jwks.json")
                    {
                        let response = Response::from_string("no")
                            .with_status_code(tiny_http::StatusCode(404));
                        let _ = request.respond(response);
                        continue;
                    }
                    let response = Response::from_string(
                        json!({"keys": [{
                            "kty": "OKP", "crv": "Ed25519", "kid": KID, "x": public_key
                        }]})
                        .to_string(),
                    )
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
                    let _ = request.respond(response);
                }
            }
            Ok(())
        }));
        rig.port = free_port(3155..3200);
        let (ready, wait) = std::sync::mpsc::channel();
        let board = ui::ServeOpts {
            host: "127.0.0.1".into(),
            port: rig.port,
            allow_hosts: vec![HOST.into()],
            board_public_only: true,
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
        let pm_dir = pm.dir.clone();
        rig.threads
            .push(thread::spawn(move || ui::serve(&state, &pm_dir, &board)));
        wait.recv_timeout(Duration::from_secs(30))
            .expect("board startup disconnected")
            .unwrap();
        rig
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }

    fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let state = self.state();
        scoped(Asserted::Operator, || client::rpc(&state, method, params))
    }

    fn claims(&self, jti: &str, actor: &str, role: &str, scopes: &[&str]) -> Value {
        let now = cadence_agent::issue::time::now_epoch();
        let grants: Vec<Value> = scopes
            .iter()
            .map(|scope| {
                json!({
                    "scope": scope,
                    "prefixes": [READ_PREFIX]
                })
            })
            .collect();
        json!({
            "iss": self.issuer,
            "aud": format!("https://{HOST}"),
            "sub": "usr_cad1467",
            "actor": actor,
            "principal_kind": "user",
            "role_at_issue": role,
            "organization_id": ORG,
            "scope": scopes,
            "wiki_prefixes": [READ_PREFIX],
            "prefix_grants": grants,
            "credential_id": "hcp_cad1467",
            "iat": now,
            "exp": now + 120,
            "jti": jti
        })
    }

    fn sign(&self, claims: &Value) -> String {
        let header = URL_SAFE_NO_PAD.encode(
            json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID}).to_string(),
        );
        let payload = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{header}.{payload}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        )
    }

    fn http(&self, path: &str, env: &str, peer: &str) -> (u16, String, Vec<u8>) {
        let mut socket = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: {HOST}\r\nContent-Length: 0\r\nConnection: close\r\n"
        )
        .unwrap();
        write!(
            socket,
            "X-Cadence-Test-As: {peer}\r\nX-Cadence-Test-Token: {}\r\n",
            self.seam
        )
        .unwrap();
        write!(socket, "Authorization: Bearer wikienv_{env}\r\n\r\n").unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let at = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("HTTP headers");
        let headers = String::from_utf8_lossy(&response[..at]).into_owned();
        let status = headers
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (status, headers, response[at + 4..].to_vec())
    }
}

fn plant(rig: &Rig, path: &str, bytes: &[u8], if_rev: &str) -> Value {
    let uploads = rig.state().join("wiki-uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let tmp = uploads.join(format!("stage-{}", bytes[0]));
    std::fs::write(&tmp, bytes).unwrap();
    rig.rpc(
        "wiki_put_blob",
        json!({
            "path": path,
            "tmp": tmp.to_str().unwrap(),
            "if_rev": if_rev
        }),
    )
    .unwrap_or_else(|error| panic!("plant {path}: {error}"))
}

fn pointer_rev(rig: &Rig, path: &str) -> String {
    let pm = Pm::at(&rig.pm).unwrap();
    let pointer = wiki::vault_dir(&pm).unwrap().join(format!("{path}.blob"));
    fnv1a(&std::fs::read(&pointer).unwrap_or_else(|error| panic!("pointer {path}: {error}")))
}

fn blob_on_disk(rig: &Rig, sha: &str) -> Vec<u8> {
    let pm = Pm::at(&rig.pm).unwrap();
    let path = wiki::blobs_dir(&wiki::vault_dir(&pm).unwrap()).join(sha);
    std::fs::read(&path).unwrap_or_else(|error| panic!("blob {sha}: {error}"))
}

fn file_query(path: &str, extra: &str) -> String {
    let mut query = format!("/api/wiki/file?path={}", url_encode(path));
    if !extra.is_empty() {
        query.push('&');
        query.push_str(extra);
    }
    query
}

fn url_encode(path: &str) -> String {
    path.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn range_query(rev: &str) -> String {
    format!("offset={START}&bytes={LEN}&rev={rev}")
}

fn json_body(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap_or_else(|_| json!({"raw": String::from_utf8_lossy(body)}))
}

fn check(failures: &mut Vec<String>, name: &str, ok: bool, detail: impl std::fmt::Display) {
    println!(
        "{} {name}: {detail}",
        if ok { "OBSERVED_OK" } else { "OBSERVED_FAIL" }
    );
    if !ok {
        failures.push(name.into());
    }
}

fn header_has(headers: &str, name: &str, needle: &str) -> bool {
    headers.lines().any(|line| {
        line.len() > name.len()
            && line[..name.len()].eq_ignore_ascii_case(name)
            && line.as_bytes().get(name.len()) == Some(&b':')
            && line.to_ascii_lowercase().contains(needle)
    })
}

fn expect_blob_slice(value: &Value, path: &str, bytes: &[u8], rev: &str) -> Result<(), String> {
    let want = [
        "data_base64",
        "kind",
        "mime",
        "name",
        "path",
        "range",
        "rev",
        "sha256",
        "size",
    ];
    if keys_of(value) != want {
        return Err(format!("keys {:?}", keys_of(value)));
    }
    if value["kind"] != "blob" || value["path"] != path || value["rev"] != rev {
        return Err(format!("identity {value}"));
    }
    if value["sha256"] != sha256_hex(bytes)
        || value["size"] != json!(bytes.len())
        || value["mime"] != "application/octet-stream"
        || value["name"] != path.rsplit('/').next().unwrap()
    {
        return Err(format!("pointer {value}"));
    }
    let range_keys = ["end", "start", "total"];
    if keys_of(&value["range"]) != range_keys {
        return Err(format!("range keys {:?}", keys_of(&value["range"])));
    }
    let end = START + LEN;
    if value["range"]["start"] != json!(START)
        || value["range"]["end"] != json!(end)
        || value["range"]["total"] != json!(bytes.len())
    {
        return Err(format!("range {}", value["range"]));
    }
    let encoded = value["data_base64"].as_str().unwrap_or_default();
    let decoded = STANDARD
        .decode(encoded)
        .map_err(|error| format!("data_base64: {error}"))?;
    if STANDARD.encode(&decoded) != encoded || decoded != slice_of(bytes) {
        return Err("data_base64 is not the exact standard-base64 slice".into());
    }
    Ok(())
}

fn meta_only(value: &Value, path: &str, bytes: &[u8], rev: &str) -> bool {
    let want = ["kind", "mime", "name", "path", "rev", "sha256", "size"];
    keys_of(value) == want
        && value["kind"] == "blob"
        && value["path"] == path
        && value["rev"] == rev
        && value["sha256"] == sha256_hex(bytes)
        && value["size"] == json!(bytes.len())
        && value["mime"] == "application/octet-stream"
        && value.get("data_base64").is_none()
        && value.get("range").is_none()
        && value.get("text").is_none()
}

#[test]
fn signed_blob_read_refuses_out_of_prefix_and_stale_rev_before_bytes() {
    let rig = Rig::start();
    let mut failures = Vec::new();
    let covered = covered_bytes();
    let outside = outside_bytes();
    let planted = plant(&rig, COVERED, &covered, wiki::ABSENT);
    let outside_planted = plant(&rig, OUTSIDE, &outside, wiki::ABSENT);
    let covered_rev = pointer_rev(&rig, COVERED);
    let outside_rev = pointer_rev(&rig, OUTSIDE);
    let covered_sha = sha256_hex(&covered);
    let outside_sha = sha256_hex(&outside);
    check(
        &mut failures,
        "operator plant commits original octet-stream bytes",
        planted.get("conflict").is_none()
            && planted["sha256"] == covered_sha
            && planted["rev"] == covered_rev
            && planted["mime"] == "application/octet-stream"
            && outside_planted["sha256"] == outside_sha
            && blob_on_disk(&rig, &covered_sha) == covered
            && blob_on_disk(&rig, &outside_sha) == outside,
        format!("covered={planted} outside={outside_planted}"),
    );

    let note = rig
        .rpc(
            "wiki_write",
            json!({"path": NOTE, "text": NOTE_TEXT, "if_rev": wiki::ABSENT}),
        )
        .unwrap_or_else(|error| panic!("text plant: {error}"));
    check(
        &mut failures,
        "operator text plant commits",
        note["committed"] == true,
        format!("{note}"),
    );

    let owner = |jti: &str| rig.sign(&rig.claims(jti, "operator", "owner", &["wiki.read"]));
    let meta = rig.http(&file_query(COVERED, ""), &owner("meta"), "operator");
    let meta_json = json_body(&meta.2);
    check(
        &mut failures,
        "signed no-range HTTP read stays pointer metadata",
        meta.0 == 200
            && header_has(&meta.1, "content-type", "application/json")
            && meta_only(&meta_json, COVERED, &covered, &covered_rev)
            && !leaks(&meta.2, &[&covered[..], slice_of(&covered)])
            && meta.2.len() + 1 < NATIVE_TOOL_LINE_BOUND,
        format!("status={} {}", meta.0, meta_json),
    );
    let rpc_meta = rig.rpc("wiki_read", json!({"path": COVERED}));
    check(
        &mut failures,
        "RPC no-range read stays pointer metadata",
        rpc_meta
            .as_ref()
            .is_ok_and(|value| meta_only(value, COVERED, &covered, &covered_rev)),
        format!("{rpc_meta:?}"),
    );

    let http_ok = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &owner("bounded"),
        "operator",
    );
    let http_json = json_body(&http_ok.2);
    let http_slice = expect_blob_slice(&http_json, COVERED, &covered, &covered_rev);
    check(
        &mut failures,
        "signed bounded HTTP read returns the exact original slice",
        http_ok.0 == 200
            && header_has(&http_ok.1, "content-type", "application/json")
            && header_has(&http_ok.1, "cache-control", "no-store")
            && http_slice.is_ok()
            && !leaks(&http_ok.2, &[&covered[..]])
            && http_ok.2.len() + 1 < NATIVE_TOOL_LINE_BOUND,
        format!("status={} {:?} {}", http_ok.0, http_slice, http_json),
    );
    let rpc_ok = rig.rpc(
        "wiki_read",
        json!({"path": COVERED, "offset": START, "bytes": LEN, "rev": covered_rev}),
    );
    check(
        &mut failures,
        "RPC bounded read matches the signed HTTP slice",
        rpc_ok.as_ref().is_ok_and(|value| value == &http_json)
            && rpc_ok.as_ref().is_ok_and(|value| {
                expect_blob_slice(value, COVERED, &covered, &covered_rev).is_ok()
            }),
        format!("{rpc_ok:?}"),
    );

    let member = rig.sign(&rig.claims("member-bounded", "users/cad1467", "member", &["wiki.read"]));
    let member_http = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &member,
        "operator",
    );
    check(
        &mut failures,
        "signed member bounded read returns the same covered slice",
        member_http.0 == 200
            && json_body(&member_http.2) == http_json
            && !leaks(&member_http.2, &[&covered[..], &outside[..]]),
        format!("status={} {}", member_http.0, json_body(&member_http.2)),
    );

    let note_http = rig.http(&file_query(NOTE, ""), &owner("text-meta"), "operator");
    let note_json = json_body(&note_http.2);
    let note_rev = fnv1a(NOTE_TEXT.as_bytes());
    check(
        &mut failures,
        "signed text read stays text",
        note_http.0 == 200
            && note_json["kind"] == "text"
            && note_json["text"] == NOTE_TEXT
            && note_json["rev"] == note_rev
            && note_json.get("data_base64").is_none(),
        format!("{note_json}"),
    );
    let note_range = rig.http(
        &file_query(NOTE, &format!("offset=0&bytes=4&rev={note_rev}")),
        &owner("text-range"),
        "operator",
    );
    let note_range_json = json_body(&note_range.2);
    check(
        &mut failures,
        "signed text range stays text and does not use data_base64",
        note_range.0 == 200
            && note_range_json["kind"] == "text"
            && note_range_json["text"] == NOTE_TEXT[..4]
            && note_range_json["range"]["start"] == json!(0)
            && note_range_json["range"]["end"] == json!(4)
            && note_range_json["range"]["total"] == json!(NOTE_TEXT.len())
            && note_range_json.get("data_base64").is_none(),
        format!("{note_range_json}"),
    );

    let stale = "fnv1a:0123456789abcdef";
    let stale_http = rig.http(
        &file_query(COVERED, &range_query(stale)),
        &owner("stale"),
        "operator",
    );
    let stale_body = String::from_utf8_lossy(&stale_http.2);
    check(
        &mut failures,
        "signed stale rev refuses with no original bytes",
        stale_http.0 == 400
            && stale_body.contains("revision")
            && stale_body.contains("not the original")
            && stale_body.contains(stale)
            && !stale_body.contains("data_base64")
            && !leaks(
                &stale_http.2,
                &[&covered[..], slice_of(&covered), &outside[..]],
            ),
        format!("status={} {stale_body}", stale_http.0),
    );
    let stale_rpc = rig.rpc(
        "wiki_read",
        json!({"path": COVERED, "offset": START, "bytes": LEN, "rev": stale}),
    );
    let stale_rpc_text = stale_rpc.as_ref().err().map(|error| error.to_string());
    check(
        &mut failures,
        "RPC stale rev refuses with no original bytes",
        stale_rpc_text.as_deref().is_some_and(|text| {
            text.contains("revision") && text.contains("not the original") && text.contains(stale)
        }) && !leaks(
            stale_rpc_text.as_deref().unwrap_or_default().as_bytes(),
            &[&covered[..], slice_of(&covered)],
        ),
        format!("{stale_rpc_text:?}"),
    );

    let outside_before = blob_on_disk(&rig, &outside_sha);
    let outside_query = file_query(OUTSIDE, &range_query(&outside_rev));
    let escaped = rig.http(&outside_query, &owner("outside"), "operator");
    let escaped_json = json_body(&escaped.2);
    check(
        &mut failures,
        "signed out-of-prefix read refuses before secret bytes",
        escaped.0 == 403
            && escaped_json["error"]["code"] == "insufficient_scope"
            && header_has(&escaped.1, "cache-control", "no-store")
            && !escaped
                .2
                .windows(outside_sha.len())
                .any(|window| window == outside_sha.as_bytes())
            && !leaks(
                &escaped.2,
                &[&outside[..], slice_of(&outside), &covered[..]],
            ),
        format!("status={} {escaped_json}", escaped.0),
    );
    let mut split = rig.claims(
        "write-prefix-cannot-read",
        "operator",
        "owner",
        &["wiki.read"],
    );
    split["scope"] = json!(["wiki.read", "wiki.write"]);
    split["wiki_prefixes"] = json!([READ_PREFIX, "global/outside/"]);
    split["prefix_grants"] = json!([
        {"scope": "wiki.read", "prefixes": [READ_PREFIX]},
        {"scope": "wiki.write", "prefixes": [READ_PREFIX, "global/outside/"]}
    ]);
    let split_http = rig.http(&outside_query, &rig.sign(&split), "operator");
    check(
        &mut failures,
        "write prefix cannot authorize an outside read",
        split_http.0 == 403
            && json_body(&split_http.2)["error"]["code"] == "insufficient_scope"
            && !leaks(&split_http.2, &[&outside[..], slice_of(&outside)]),
        format!("status={} {}", split_http.0, json_body(&split_http.2)),
    );
    let adjacent = rig.http(
        &file_query("global/covered-evil/secret.bin", &range_query(&outside_rev)),
        &owner("adjacent"),
        "operator",
    );
    check(
        &mut failures,
        "prefix-adjacent path refuses",
        adjacent.0 == 403
            && json_body(&adjacent.2)["error"]["code"] == "insufficient_scope"
            && !leaks(&adjacent.2, &[&outside[..], &covered[..]]),
        format!("status={} {}", adjacent.0, json_body(&adjacent.2)),
    );
    let dotdot = rig.http(
        &file_query(
            "global/covered/../outside/secret.bin",
            &range_query(&outside_rev),
        ),
        &owner("dotdot"),
        "operator",
    );
    let dotdot_text = String::from_utf8_lossy(&dotdot.2);
    check(
        &mut failures,
        "dotdot read refuses before secret bytes",
        (400..500).contains(&dotdot.0)
            && dotdot_text.contains("'.' and '..'")
            && !leaks(&dotdot.2, &[&outside[..], slice_of(&outside)]),
        format!("status={} {dotdot_text}", dotdot.0),
    );
    let pointer_path = rig.http(
        &file_query("global/covered/proof.bin.blob", &range_query(&covered_rev)),
        &owner("pointer-path"),
        "operator",
    );
    let pointer_text = String::from_utf8_lossy(&pointer_path.2);
    check(
        &mut failures,
        "pointer path refuses before blob bytes",
        (400..500).contains(&pointer_path.0)
            && pointer_text.contains(".blob")
            && !leaks(&pointer_path.2, &[&covered[..], slice_of(&covered)]),
        format!("status={} {pointer_text}", pointer_path.0),
    );
    let direct = rig.rpc(
        "wiki_read",
        json!({
            "path": format!(".blobs/{covered_sha}"),
            "offset": START,
            "bytes": LEN,
            "rev": covered_rev
        }),
    );
    let direct_text = direct.as_ref().err().map(|error| error.to_string());
    check(
        &mut failures,
        "direct blob object path refuses before original bytes",
        direct_text
            .as_deref()
            .is_some_and(|text| text.contains("dot-prefixed"))
            && !leaks(
                direct_text.as_deref().unwrap_or_default().as_bytes(),
                &[&covered[..], slice_of(&covered)],
            ),
        format!("{direct_text:?}"),
    );

    let write_only = rig.sign(&rig.claims("write-only", "operator", "owner", &["wiki.write"]));
    let write_only_http = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &write_only,
        "operator",
    );
    check(
        &mut failures,
        "wiki.write does not authorize a blob read",
        write_only_http.0 == 403
            && json_body(&write_only_http.2)["error"]["code"] == "insufficient_scope"
            && !leaks(&write_only_http.2, &[&covered[..], slice_of(&covered)]),
        format!(
            "status={} {}",
            write_only_http.0,
            json_body(&write_only_http.2)
        ),
    );
    let mut service = rig.claims("service-actor", "operator", "owner", &["wiki.read"]);
    service["actor"] = json!("service/cad1467");
    service["principal_kind"] = json!("service");
    service["role_at_issue"] = json!("member");
    let service_http = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &rig.sign(&service),
        "operator",
    );
    let service_json = json_body(&service_http.2);
    check(
        &mut failures,
        "service actor refuses before blob bytes",
        service_http.0 == 403
            && service_json["error"]["code"] == "insufficient_scope"
            && service_json["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("no wiki identity"))
            && !leaks(&service_http.2, &[&covered[..], slice_of(&covered)]),
        format!("status={} {service_json}", service_http.0),
    );

    let replay_token = owner("replay-once");
    let replay_first = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &replay_token,
        "operator",
    );
    let replay_second = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &replay_token,
        "operator",
    );
    check(
        &mut failures,
        "replay of a successful bounded read releases no second copy",
        replay_first.0 == 200
            && json_body(&replay_first.2) == http_json
            && replay_second.0 == 401
            && json_body(&replay_second.2)["error"]["code"] == "assertion_replayed"
            && !leaks(&replay_second.2, &[&covered[..], slice_of(&covered)]),
        format!(
            "first={} second={} {}",
            replay_first.0,
            replay_second.0,
            json_body(&replay_second.2)
        ),
    );
    let real = owner("tamper");
    let mut parts: Vec<String> = real.split('.').map(str::to_owned).collect();
    let mut claims = rig.claims("tamper", "operator", "owner", &["wiki.read"]);
    claims["sub"] = json!("usr_other");
    parts[1] = URL_SAFE_NO_PAD.encode(claims.to_string());
    let tampered = parts.join(".");
    let tamper_http = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &tampered,
        "operator",
    );
    check(
        &mut failures,
        "tampered signature releases no blob bytes",
        tamper_http.0 == 401
            && json_body(&tamper_http.2)["error"]["code"] == "signature_invalid"
            && !leaks(
                &tamper_http.2,
                &[&covered[..], slice_of(&covered), &outside[..]],
            ),
        format!("status={} {}", tamper_http.0, json_body(&tamper_http.2)),
    );

    for (peer, needle, name) in [
        (
            "agent:cad1467foreign",
            "never acts as another",
            "HTTP agent peer releases no blob bytes",
        ),
        (
            "unproven",
            "derives no agent identity",
            "HTTP unproven peer releases no blob bytes",
        ),
    ] {
        let response = rig.http(
            &file_query(COVERED, &range_query(&covered_rev)),
            &owner(&format!("peer-{peer}")),
            peer,
        );
        let text = String::from_utf8_lossy(&response.2);
        check(
            &mut failures,
            name,
            (400..500).contains(&response.0)
                && text.contains(needle)
                && !text.contains("data_base64")
                && !leaks(&response.2, &[&covered[..], slice_of(&covered)]),
            format!("status={} {text}", response.0),
        );
    }
    let state = rig.state();
    let forged = scoped(Asserted::Agent("cad1467foreign".into()), || {
        client::rpc(
            &state,
            "wiki_read",
            json!({
                "path": COVERED,
                "offset": START,
                "bytes": LEN,
                "rev": covered_rev,
                "wiki_as": "operator"
            }),
        )
    });
    let forged_text = forged.as_ref().err().map(|error| error.to_string());
    check(
        &mut failures,
        "agent RPC cannot forge wiki_as operator to read blob bytes",
        forged_text
            .as_deref()
            .is_some_and(|text| text.contains("never acts as another"))
            && !leaks(
                forged_text.as_deref().unwrap_or_default().as_bytes(),
                &[&covered[..], slice_of(&covered)],
            ),
        format!("{forged_text:?}"),
    );
    check(
        &mut failures,
        "refusals leave the outside blob bytes unchanged",
        blob_on_disk(&rig, &outside_sha) == outside_before && outside_before == outside,
        "outside blob changed",
    );

    let replaced = replaced_bytes();
    let replaced_plant = plant(&rig, COVERED, &replaced, &covered_rev);
    let replaced_rev = pointer_rev(&rig, COVERED);
    let replaced_sha = sha256_hex(&replaced);
    check(
        &mut failures,
        "operator replacement commits a new pointer revision",
        replaced_plant.get("conflict").is_none()
            && replaced_plant["rev"] == replaced_rev
            && replaced_rev != covered_rev
            && blob_on_disk(&rig, &replaced_sha) == replaced,
        format!("{replaced_plant}"),
    );
    let moved_http = rig.http(
        &file_query(COVERED, &range_query(&covered_rev)),
        &owner("moved"),
        "operator",
    );
    let moved_text = String::from_utf8_lossy(&moved_http.2);
    check(
        &mut failures,
        "HTTP previous rev refuses after replacement with no bytes",
        moved_http.0 == 400
            && moved_text.contains("revision")
            && moved_text.contains("not the original")
            && moved_text.contains(&covered_rev)
            && !leaks(
                &moved_http.2,
                &[
                    &covered[..],
                    slice_of(&covered),
                    &replaced[..],
                    slice_of(&replaced),
                ],
            ),
        format!("status={} {moved_text}", moved_http.0),
    );
    let moved_rpc = rig.rpc(
        "wiki_read",
        json!({"path": COVERED, "offset": START, "bytes": LEN, "rev": covered_rev}),
    );
    let moved_rpc_text = moved_rpc.as_ref().err().map(|error| error.to_string());
    check(
        &mut failures,
        "RPC previous rev refuses after replacement with no bytes",
        moved_rpc_text.as_deref().is_some_and(|text| {
            text.contains("revision")
                && text.contains("not the original")
                && text.contains(&covered_rev)
        }) && !leaks(
            moved_rpc_text.as_deref().unwrap_or_default().as_bytes(),
            &[
                &covered[..],
                slice_of(&covered),
                &replaced[..],
                slice_of(&replaced),
            ],
        ),
        format!("{moved_rpc_text:?}"),
    );
    let fresh = rig.http(
        &file_query(COVERED, &range_query(&replaced_rev)),
        &owner("fresh"),
        "operator",
    );
    let fresh_json = json_body(&fresh.2);
    check(
        &mut failures,
        "signed read of the new rev returns only the replacement slice",
        fresh.0 == 200
            && expect_blob_slice(&fresh_json, COVERED, &replaced, &replaced_rev).is_ok()
            && !leaks(&fresh.2, &[&covered[..], slice_of(&covered), &replaced[..]]),
        format!("status={} {fresh_json}", fresh.0),
    );

    assert!(
        failures.is_empty(),
        "CAD-1467 blob read guard failures: {failures:?}"
    );
}
