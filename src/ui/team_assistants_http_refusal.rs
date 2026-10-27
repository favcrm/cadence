//! CAD-1380 independent refusal acceptance — the REAL `/api/assist/<verb>`
//! HTTP admission path, authored by a worker that is NOT the implementer. The
//! implementer must not edit or weaken it; `mod team_assistants_http_refusal;`
//! in `src/ui.rs` is the implementer's registration step (this worker does not
//! touch product wiring).
//!
//! Unlike a relay stub, this drives the ACTUAL container surface end to end:
//! a real `crate::daemon::serve_with` daemon on a real Unix socket, a real
//! `crate::ui::serve` board bound on a 3110–3199 loopback port with a
//! configured `public` board identity, a real platform authority server on a
//! second loopback port answering BOTH the JWKS fetch and the
//! `/__platform/assistant/revalidate` live-authority pull, and real
//! `POST /api/assist/<verb>` requests over a TCP socket. The route verifies
//! the `agenticos-assistant-actor/1` envelope (`src/assistant_actor.rs`),
//! consumes the single-use `jti` (`assist-jtis.json`), and relays the raw
//! signed envelope to the daemon's `member_*` verbs over the real
//! `client::rpc` socket — where the daemon re-verifies the cryptography,
//! re-binds to the stored `assistants` row, and re-proves the member's
//! CURRENT platform authority through the revalidate pull before any
//! effect. No step is a fake mirroring the code under test.
//!
//! The parity claim under test (AGENTS.md "a board or HTTP path must be at
//! least as strict as the daemon RPC it relays"): the same forged / wrong-org /
//! wrong-audience / expired / replayed / wrong-scope / operator-actor envelope
//! refused by `member_*`/`assistant_actor::verify` is refused at the HTTP
//! boundary BEFORE it can produce an effect, a verified member envelope
//! reaches its own bound assistant exactly once, and a member whose
//! AUTHORITATIVE platform membership is withdrawn after a request was
//! admitted cannot commit a second effect (the removal lives at the
//! platform — nothing pokes the local `assistants.state` flag).
//!
//! Every refused case is paired with a positive control in the same fixture so
//! a blanket-fail cannot pass, and "no effect on refusal" is asserted against a
//! whole-store byte snapshot.

#![cfg(all(test, feature = "test-seam"))]

use super::*;
use crate::daemon::team_assistants_refusal::AssistantFlagOn;
use crate::daemon::ServeOptions;
use crate::test_seam::Seam;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use tiny_http::{Header, Response, Server};

const ORG: &str = "c1380-org";
const KID: &str = "c1380-http-fixture";

// Child placement reuses private fixture helpers without exporting them or
// changing production module registration. The parent file is test-seam only.
#[path = "cad1485_member_chat_refusal.rs"]
mod cad1485_member_chat_refusal;

fn loopback_server() -> Server {
    (3110..=3199)
        .find_map(|port| Server::http(("127.0.0.1", port)).ok())
        .expect("no free isolated fixture port in 3110..3199")
}

/// The authoritative member record the fixture platform holds — the
/// `workspace_members` + `personalAssistants` + issuing-credential state
/// the real endpoint re-reads.
#[derive(Clone)]
struct MemberAuthority {
    role: Option<&'static str>,
    binding_active: bool,
    recorded_credential: String,
    credential_live: bool,
    credential_caps: Vec<String>,
}

impl MemberAuthority {
    fn live(sub: &str) -> Self {
        Self {
            role: Some("member"),
            binding_active: true,
            recorded_credential: format!("cred-{sub}"),
            credential_live: true,
            credential_caps: vec![
                "assist.read".into(),
                "assist.write".into(),
                "assist.jobs".into(),
            ],
        }
    }
}

#[derive(Default)]
struct Authority {
    members: BTreeMap<String, MemberAuthority>,
    /// Serve a non-200 status (platform failure — fail closed).
    status: u16,
    /// Opt-in response recorder for CAD-1485 only. None preserves the old rig.
    board_authorize: Option<cad1485_member_chat_refusal::BoardAuthorize>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevalidateRequest {
    version: String,
    organization_id: String,
    sub: String,
    assistant_id: String,
    operation: String,
    scope: String,
    resource: Value,
}

#[derive(serde::Deserialize)]
struct EnvelopeClaims {
    iss: String,
    aud: String,
    sub: String,
    actor: String,
    principal_kind: String,
    role_at_issue: String,
    organization_id: String,
    assistant_id: String,
    scope: Vec<String>,
    credential_id: String,
    iat: i64,
    exp: i64,
    #[allow(dead_code)]
    jti: String,
}

/// The authoritative platform endpoint — the same decision the real v2
/// `revalidateAssistantMember` computes: a member-envelope bearer must
/// re-verify (signature + pins) and name THIS member; `allow` then comes
/// from CURRENT member state — live membership + member-facing role,
/// active binding, the recorded issuing credential live with its grant
/// ceiling covering the requested scope, and the envelope's provenance
/// (`credential_id` = the binding's recorded credential) plus its own
/// scope covering the operation.
fn answer_platform(
    mut request: tiny_http::Request,
    authority: &Mutex<Authority>,
    issuer: &str,
    host: &str,
    jwks_public: &str,
    verify_public: &[u8],
) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/").to_string();
    let respond = |request: tiny_http::Request, status: u16, body: Value| {
        let response = Response::from_string(body.to_string())
            .with_status_code(tiny_http::StatusCode(status))
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        let _ = request.respond(response);
    };
    if path == "/.well-known/agenticos-board-jwks.json" {
        respond(
            request,
            200,
            json!({"keys":[{"kty":"OKP","crv":"Ed25519","kid":KID,"x":jwks_public}]}),
        );
        return;
    }
    if path == "/__platform/assistant/board-authorize" {
        let mut guard = authority.lock().unwrap();
        if let Some(reply) = guard.board_authorize.as_mut() {
            reply.answer(request);
            return;
        }
    }
    if path != "/__platform/assistant/revalidate" {
        respond(request, 404, json!({"ok":false,"code":"not_found"}));
        return;
    }
    {
        let guard = authority.lock().unwrap();
        if guard.status != 200 {
            let status = guard.status;
            drop(guard);
            respond(request, status, json!({"ok":false,"code":"unavailable"}));
            return;
        }
    }
    let mut body = Vec::new();
    if request
        .as_reader()
        .take(64 * 1024)
        .read_to_end(&mut body)
        .is_err()
    {
        respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
        return;
    }
    let input: RevalidateRequest = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
            return;
        }
    };
    let scope_ok = crate::assistant_actor::AssistVerb::of(&input.operation).is_some_and(|v| {
        let want = match v.scope() {
            crate::assistant_actor::AssistScope::Read => "assist.read",
            crate::assistant_actor::AssistScope::Write => "assist.write",
            crate::assistant_actor::AssistScope::Jobs => "assist.jobs",
        };
        input.scope == want
    });
    if input.version != "hosted-cadence-assistant.v1"
        || input.organization_id != ORG
        || !scope_ok
        || input.resource["kind"] != "assistant"
    {
        respond(request, 400, json!({"ok":false,"code":"invalid_request"}));
        return;
    }
    let bearer = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    let member = {
        let guard = authority.lock().unwrap();
        guard.members.get(&input.sub).cloned()
    };
    let (envelope_credential, envelope_scopes): (Option<String>, Vec<String>) =
        if let Some(env) = bearer.strip_prefix("Bearer wikienv_") {
            match verify_envelope(env, verify_public, issuer, host) {
                Some(claims)
                    if claims.sub == input.sub
                        && claims.assistant_id == input.assistant_id
                        && claims.organization_id == input.organization_id =>
                {
                    (Some(claims.credential_id), claims.scope)
                }
                _ => {
                    respond(request, 401, json!({"ok":false,"code":"unauthorized"}));
                    return;
                }
            }
        } else {
            respond(request, 401, json!({"ok":false,"code":"unauthorized"}));
            return;
        };
    let allow = member.is_some_and(|m| {
        let role_ok = matches!(m.role, Some("member") | Some("owner"));
        let credential_ok =
            m.credential_live && m.credential_caps.iter().any(|c| c == &input.scope);
        let provenance_ok = match &envelope_credential {
            Some(cred) => {
                cred == &m.recorded_credential && envelope_scopes.iter().any(|s| s == &input.scope)
            }
            None => true,
        };
        role_ok
            && m.binding_active
            && input.assistant_id == format!("assist_{}", input.sub)
            && credential_ok
            && provenance_ok
    });
    respond(request, 200, json!({"allow": allow}));
}

/// The authoritative provider's own envelope verify — signature + pins.
fn verify_envelope(
    envelope: &str,
    verify_public: &[u8],
    issuer: &str,
    host: &str,
) -> Option<EnvelopeClaims> {
    let parts: Vec<&str> = envelope.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).ok()?).ok()?;
    if header["alg"] != "EdDSA"
        || header["typ"] != "agenticos-assistant-actor/1"
        || header["kid"] != KID
    {
        return None;
    }
    let claims: EnvelopeClaims =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).ok()?).ok()?;
    let signature = URL_SAFE_NO_PAD.decode(parts[2]).ok()?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, verify_public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .ok()?;
    let now = crate::issue::time::now_epoch();
    if claims.iss != issuer
        || claims.aud != format!("http://{host}")
        || claims.organization_id != ORG
        || claims.iat > now + 30
        || claims.exp <= now
        || claims.exp - claims.iat > 300
        || claims.principal_kind != "user"
        || claims.actor != format!("users/{}", claims.sub)
        || claims.assistant_id != format!("assist_{}", claims.sub)
        || !matches!(claims.role_at_issue.as_str(), "member" | "owner")
        || claims
            .scope
            .iter()
            .any(|s| !matches!(s.as_str(), "assist.read" | "assist.write" | "assist.jobs"))
    {
        return None;
    }
    Some(claims)
}

/// A running end-to-end fixture: real daemon socket + real board + the
/// real platform authority server (JWKS + revalidate).
struct Rig {
    _dir: tempfile::TempDir,
    pm: PathBuf,
    board_port: u16,
    /// The platform's public host — `localhost:<port>` so the daemon's
    /// revalidate pull can dial it while keeping the contract's http
    /// scheme derivation for a localhost host.
    public_host: String,
    issuer: String,
    key: Ed25519KeyPair,
    authority: Arc<Mutex<Authority>>,
    stop: Arc<AtomicBool>,
    _threads: Vec<std::thread::JoinHandle<()>>,
}

impl Rig {
    /// Mint an `agenticos-assistant-actor/1` envelope; `claims`/`header`
    /// overrides forge exactly one field.
    fn envelope(&self, sub: &str, header: Value, claims: Value) -> String {
        let now = crate::issue::time::now_epoch();
        let mut h = json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID});
        if let Some(o) = header.as_object() {
            for (k, v) in o {
                h[k] = v.clone();
            }
        }
        let mut c = json!({
            "iss": self.issuer, "aud": format!("http://{}", self.public_host),
            "sub": sub, "actor": format!("users/{sub}"), "principal_kind": "user",
            "role_at_issue": "member", "organization_id": ORG,
            "assistant_id": format!("assist_{sub}"),
            "scope": ["assist.read","assist.write","assist.jobs"],
            "credential_id": format!("cred-{sub}"),
            "iat": now, "exp": now + 120,
            "jti": format!("jti-{sub}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos())
        });
        if let Some(o) = claims.as_object() {
            for (k, v) in o {
                c[k] = v.clone();
            }
        }
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&h).unwrap()),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&c).unwrap())
        );
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(self.key.sign(signed.as_bytes()).as_ref())
        )
    }

    /// `POST /api/assist/<verb>` on the public host → (status, body). The
    /// Host header names the public board identity so `handle` diverts the
    /// request into the assist family — the same surface the platform
    /// relay forwards to.
    fn call(&self, verb: &str, envelope: &str, args: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(args).unwrap();
        let mut socket = TcpStream::connect(("127.0.0.1", self.board_port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(
            socket,
            "POST /api/assist/{verb} HTTP/1.1\r\nHost: {}\r\n\
             Authorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            self.public_host,
            body.len()
        )
        .unwrap();
        socket.write_all(&body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("no http header/body split");
        let header = std::str::from_utf8(&response[..split]).unwrap();
        let status = header
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let value = serde_json::from_slice(&response[split + 4..]).unwrap_or_else(
            |_| json!({"_nonjson": String::from_utf8_lossy(&response[split+4..]).into_owned()}),
        );
        (status, value)
    }
}

/// Boot the real daemon + real board + the real platform authority server
/// (JWKS + `/__platform/assistant/revalidate`). `subs` are the members to
/// provision: a stored `assistants` binding per sub (the daemon-side row)
/// plus a LIVE authoritative member record at the platform — the two
/// halves of a provisioned member.
fn start(subs: &[&str]) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("c1380-http")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();

    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    // Real daemon on a real Unix socket — it owns the singleton, applies
    // migrations and mints the seam token.
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // The authoritative platform server — JWKS + the revalidate pull —
    // BEFORE the board so `public.host` can name it. `localhost:<port>`
    // keeps the contract's http scheme derivation for the host AND is
    // dialable by the daemon's revalidation client.
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let verify_public = verify_key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound an unexpected address"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    {
        let stop = stop.clone();
        let authority = authority.clone();
        let issuer = issuer.clone();
        let public_host = public_host.clone();
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                answer_platform(
                    request,
                    &authority,
                    &issuer,
                    &public_host,
                    &public_key,
                    &verify_public,
                );
            }
        }));
    }

    // Provision each member: the stored `assistants` binding the daemon
    // re-binds to (a second `Shared` opens alongside the live daemon) AND
    // the live authoritative member record the revalidate pull reads.
    {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = crate::daemon::Shared::new(&state, &opts).unwrap();
        for sub in subs {
            let assistant_id = format!("assist_{sub}");
            shared
                .store
                .assistant_bind(
                    ORG,
                    sub,
                    &assistant_id,
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap_or_else(|e| panic!("provision {sub}: {e}"));
            authority
                .lock()
                .unwrap()
                .members
                .insert(sub.to_string(), MemberAuthority::live(sub));
        }
    }

    // Real board with a public identity so `/api/assist/*` diverts. The
    // public host names the platform (`localhost:<port>`) so the daemon's
    // `board-identity.json` — which `ui::serve` writes from this config —
    // points the revalidation pull at the fixture authority. The Host
    // allowlist carries it so the public-surface branch engages.
    let mut board_port = 0u16;
    for port in 3110..=3199u16 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            ..ServeOpts::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "board could not bind in 3110..=3199");

    Rig {
        _dir: dir,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        authority,
        stop,
        _threads: threads,
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// A whole-store byte snapshot — the "no effect" probe for refused HTTP calls.
fn tree(root: &std::path::Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(
        base: &std::path::Path,
        dir: &std::path::Path,
        out: &mut std::collections::BTreeMap<String, Vec<u8>>,
    ) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if rel == ".git" {
                continue;
            }
            if p.is_dir() {
                out.insert(format!("{rel}/"), vec![]);
                walk(base, &p, out);
            } else {
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// The focused HTTP-parity scenario: a verified member envelope reaches its
/// own assistant and requests a job (positive control), and every forged /
/// wrong-org / wrong-audience / expired / replayed / wrong-scope / operator /
/// identity-field variant is refused at the real `/api/assist/*` boundary
/// with no side effect — matching the daemon's `member_*`/`assistant_actor`
/// guard.
#[test]
fn assist_http_admission_refuses_forged_org_aud_expired_replay_scope_operator() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let good = rig.envelope("alice", json!({}), json!({}));

    // Positive control: a verified member envelope admits assist_state and
    // the daemon's member_* handler resolves the bound assistant — through
    // the live revalidation pull.
    let (status, body) = rig.call("assist_state", &good, &json!({}));
    assert_eq!(
        status, 200,
        "valid member envelope refused at HTTP admission: {body}"
    );

    let before = tree(&rig.pm);

    // Forged signature — signed by a different key.
    let other_key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let forged = {
        let now = crate::issue::time::now_epoch();
        let h = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(
                &json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID}),
            )
            .unwrap(),
        );
        let c = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": rig.issuer, "aud": format!("http://{}", rig.public_host),
                "sub":"alice","actor":"users/alice","principal_kind":"user",
                "role_at_issue":"member","organization_id":ORG,
                "assistant_id":"assist_alice","scope":["assist.read"],
                "credential_id":"cred-alice","iat":now,"exp":now+120,"jti":"jti-forged"
            }))
            .unwrap(),
        );
        let signed = format!("{h}.{c}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(other_key.sign(signed.as_bytes()).as_ref())
        )
    };
    let (status, body) = rig.call("assist_state", &forged, &json!({}));
    assert_eq!(status, 401, "forged signature admitted: {body}");
    assert_eq!(tree(&rig.pm), before, "forged-sig call had an effect");

    // Wrong org.
    let env = rig.envelope("alice", json!({}), json!({"organization_id": "other-org"}));
    let (status, body) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(status, 401, "wrong-org envelope admitted: {body}");
    assert_eq!(tree(&rig.pm), before);

    // Wrong audience.
    let env = rig.envelope(
        "alice",
        json!({}),
        json!({"aud": "http://other.board.localhost"}),
    );
    let (status, body) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(status, 401, "wrong-audience envelope admitted: {body}");
    assert_eq!(tree(&rig.pm), before);

    // Expired.
    let now = crate::issue::time::now_epoch();
    let env = rig.envelope(
        "alice",
        json!({}),
        json!({"iat": now - 400, "exp": now - 100}),
    );
    let (status, body) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(status, 401, "expired envelope admitted: {body}");
    assert_eq!(tree(&rig.pm), before);

    // Operator actor on the member family.
    let env = rig.envelope(
        "alice",
        json!({}),
        json!({"actor":"operator","role_at_issue":"owner"}),
    );
    let (status, body) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(
        status, 401,
        "operator-actor assistant envelope admitted: {body}"
    );
    assert_eq!(tree(&rig.pm), before);

    // assistant_id not bound to sub.
    let env = rig.envelope(
        "alice",
        json!({}),
        json!({"assistant_id": "assist_mallory"}),
    );
    let (status, body) = rig.call("assist_state", &env, &json!({}));
    assert_eq!(status, 401, "assistant_id mismatch admitted: {body}");
    assert_eq!(tree(&rig.pm), before);

    // Caller-supplied identity field in the arguments body.
    for field in [
        "actor",
        "principal",
        "org",
        "as",
        "assistant_id",
        "wiki_as",
        "member_as",
    ] {
        let env = rig.envelope("dave", json!({}), json!({}));
        let (status, body) = rig.call("assist_send", &env, &json!({field: "mallory", "text": "x"}));
        assert!(
            status == 400 || status == 401,
            "identity field {field} not refused: {body}"
        );
        assert_eq!(
            tree(&rig.pm),
            before,
            "identity field {field} had an effect"
        );
    }

    // Replay: the `good` envelope's jti is spent.
    let (status, body) = rig.call("assist_state", &good, &json!({}));
    assert_eq!(status, 401, "replayed jti admitted at HTTP: {body}");
    assert_eq!(tree(&rig.pm), before, "replayed envelope had an effect");

    // Unlisted verb refused before the body is read.
    let env = rig.envelope("erin", json!({}), json!({}));
    let (status, _b) = rig.call("merge", &env, &json!({}));
    assert!(
        status == 403 || status == 404,
        "unlisted assist verb admitted: {_b}"
    );
    assert_eq!(tree(&rig.pm), before);

    // Insufficient scope: assist.read-only on the write/jobs verbs.
    let env = rig.envelope("frank", json!({}), json!({"scope": ["assist.read"]}));
    for (verb, args) in [
        ("assist_send", json!({"text": "x"})),
        ("job_request", json!({"title": "t", "request_key": "k"})),
    ] {
        let (status, body) = rig.call(verb, &env, &args);
        assert_eq!(status, 403, "read-scope ran {verb} at HTTP: {body}");
        assert_eq!(
            tree(&rig.pm),
            before,
            "insufficient-scope {verb} had an effect"
        );
    }
}

/// Cross-principal HTTP parity: member B's envelope cannot read member A's
/// assistant surface or job evidence. The envelope's `sub`/`assistant_id`
/// are verified pins — a member can never aim the relay at another
/// principal because there is no identity field it controls.
#[test]
fn assist_http_member_b_cannot_reach_member_a() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice", "bob"]);
    let alice = rig.envelope("alice", json!({}), json!({}));
    let bob = rig.envelope("bob", json!({}), json!({}));

    // Alice requests a job through the real HTTP path (positive control).
    let (status, body) = rig.call(
        "job_request",
        &alice,
        &json!({"title": "a job", "request_key": "a-k1"}),
    );
    assert_eq!(status, 200, "alice's job_request refused at HTTP: {body}");
    let job = body["job"].as_str().unwrap_or_default().to_string();
    assert!(!job.is_empty(), "job_request returned no job: {body}");

    let before = tree(&rig.pm);

    // Bob tries to read Alice's job evidence and event feed — the daemon's
    // owner/subscriber ACL refuses because bob neither owns nor subscribes.
    for verb in ["job_evidence", "job_events"] {
        let env = rig.envelope("bob", json!({}), json!({}));
        let (status, body) = rig.call(verb, &env, &json!({"job": job}));
        assert!(
            status == 401 || status == 403,
            "bob reached alice's {verb}: {body}"
        );
        assert_eq!(tree(&rig.pm), before, "bob's foreign {verb} had an effect");
    }

    // Bob's own envelope reads bob's own assistant state (positive control).
    let (status, body) = rig.call("assist_state", &bob, &json!({}));
    assert_eq!(status, 200, "bob's own assist_state refused: {body}");
}

/// Removed-member effect on the real HTTP+daemon path — acceptance 7/R4 at
/// the AUTHORITATIVE source: the platform's membership goes away (the
/// fixture authority flips; the local `assistants.state` is never poked),
/// so the only thing that can refuse the late effect is the daemon's
/// effect-time `__platform/assistant/revalidate` pull answering
/// `allow:false`. A queued/late effect for a removed member must not
/// commit, and an unreachable/failed authority pull fails closed too.
#[test]
fn removed_member_after_enqueue_cannot_commit_an_effect() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["carol"]);
    let carol = rig.envelope("carol", json!({}), json!({}));
    let (status, body) = rig.call(
        "job_request",
        &carol,
        &json!({"title": "queued", "request_key": "c-q1"}),
    );
    assert_eq!(status, 200, "live member job_request refused: {body}");

    // The member's platform authority is withdrawn — on the authoritative
    // membership source, NOT by touching the local binding. The next
    // member effect must re-prove that authority at commit and refuse.
    rig.authority.lock().unwrap().members.remove("carol");
    let before = tree(&rig.pm);
    let removed = rig.envelope("carol", json!({}), json!({}));
    let (status, body) = rig.call(
        "job_request",
        &removed,
        &json!({"title": "late", "request_key": "c-q2"}),
    );
    assert!(
        status == 401 || status == 403,
        "removed member's late job_request committed — the effect-time \
         platform revalidation did not refuse it: {body}"
    );
    assert_eq!(tree(&rig.pm), before, "a removed member's effect committed");

    // A member whose recorded issuing credential is revoked at the
    // platform is refused on the same live pull — the ≤300 s envelope
    // window is not authority.
    let rig = start(&["dana"]);
    let (status, body) = rig.call(
        "job_request",
        &rig.envelope("dana", json!({}), json!({})),
        &json!({"title": "queued", "request_key": "d-q1"}),
    );
    assert_eq!(status, 200, "live member job_request refused: {body}");
    rig.authority
        .lock()
        .unwrap()
        .members
        .get_mut("dana")
        .unwrap()
        .credential_live = false;
    let before = tree(&rig.pm);
    let (status, body) = rig.call(
        "assist_state",
        &rig.envelope("dana", json!({}), json!({})),
        &json!({}),
    );
    assert!(
        status == 401 || status == 403,
        "credential-revoked member's read committed: {body}"
    );
    assert_eq!(tree(&rig.pm), before);

    // An unreachable/failed authority answer fails closed — the member is
    // still live at the platform but the pull returns non-200.
    rig.authority.lock().unwrap().status = 503;
    let before = tree(&rig.pm);
    let (status, body) = rig.call(
        "assist_state",
        &rig.envelope("dana", json!({}), json!({})),
        &json!({}),
    );
    assert!(
        status == 401 || status == 403 || status == 503,
        "a failed authority pull was allowed: {body}"
    );
    assert_eq!(tree(&rig.pm), before);
}

/// `/api/assist/{job_share,job_subscribe}` HTTP parity — the
/// second-member shared-job ACL over the real socket. A merely-same-org
/// member is refused `job_subscribe` (and the bound reads) with no
/// subscriber row and no effect — org membership is not job authority
/// (SEC-002). Only the OWNING member's `job_share` — the explicit
/// authoritative grant, gated by `assist_request_owns` — admits B, once
/// and idempotently; then B's own `job_evidence`/`job_events` resolve
/// over HTTP on the SAME canonical job. A non-owner can never share, a
/// non-assist job refuses, and B's subscription never reaches A's
/// private conversation.
#[test]
fn assist_http_job_subscribe_needs_owner_share_not_membership() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["ana", "ben", "cid"]);
    let ana = rig.envelope("ana", json!({}), json!({}));
    let (status, body) = rig.call(
        "job_request",
        &ana,
        &json!({"title": "shared", "request_key": "a-s1"}),
    );
    assert_eq!(status, 200, "ana's job_request refused: {body}");
    let job = body["job"].as_str().unwrap_or_default().to_string();
    assert!(!job.is_empty(), "job_request returned no job: {body}");

    // ---- B same-org, UNENTITLED (owner has not shared) → subscribe AND
    // the bound reads refuse with no effect ----
    let before = tree(&rig.pm);
    for verb in ["job_subscribe", "job_evidence", "job_events"] {
        let env = rig.envelope("ben", json!({}), json!({}));
        let (status, body) = rig.call(verb, &env, &json!({"job": job}));
        assert!(
            status == 401 || status == 403,
            "unentitled ben reached {verb}: {body}"
        );
        assert_eq!(tree(&rig.pm), before, "unentitled {verb} had an effect");
    }

    // ---- a NON-owner can never mint the entitlement: B cannot share
    // A's job. ----
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, body) = rig.call("job_share", &env, &json!({"job": job}));
    assert!(
        status == 401 || status == 403,
        "a non-owner shared A's job: {body}"
    );
    assert_eq!(tree(&rig.pm), before, "non-owner share had an effect");

    // ---- the OWNING member's share admits B — once and idempotently ----
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, body) = rig.call("job_share", &env, &json!({"job": job}));
    assert_eq!(status, 200, "owner's job_share refused: {body}");
    assert_eq!(body["shared"].as_bool(), Some(true));

    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, body) = rig.call("job_subscribe", &env, &json!({"job": job}));
    assert_eq!(
        status, 200,
        "granted ben's job_subscribe refused at HTTP: {body}"
    );
    assert_eq!(body["job"].as_str(), Some(job.as_str()));
    // Idempotent — a replayed subscribe re-binds to the same canonical job.
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, body) = rig.call("job_subscribe", &env, &json!({"job": job}));
    assert_eq!(status, 200, "replayed subscribe refused: {body}");
    assert_eq!(body["job"].as_str(), Some(job.as_str()));
    // B's own evidence + events now resolve over HTTP on the SAME job.
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, body) = rig.call("job_evidence", &env, &json!({"job": job}));
    assert_eq!(status, 200, "granted ben's evidence refused: {body}");
    let env = rig.envelope("ben", json!({}), json!({}));
    let (status, body) = rig.call("job_events", &env, &json!({"job": job}));
    assert_eq!(status, 200, "granted ben's events refused: {body}");
    let events_body = body;

    // ---- a non-assist job refuses subscribe at the daemon's assist-
    // binding check. ----
    let env = rig.envelope("ben", json!({}), json!({}));
    let before = tree(&rig.pm);
    let (status, body) = rig.call(
        "job_subscribe",
        &env,
        &json!({"job": "assist-0000000000000000000000ff"}),
    );
    assert!(
        status == 401 || status == 403 || status == 400,
        "a non-assist job id subscribed: {body}"
    );
    assert_eq!(tree(&rig.pm), before, "non-assist subscribe had an effect");

    // ---- A/B privacy: B's job event feed is job metadata only — no
    // private conversation content crosses the subscription boundary. ----
    for e in events_body["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        let s = e.to_string();
        for forbidden in ["assist_history", "private body"] {
            assert!(
                !s.contains(forbidden),
                "B's job events leaked private content: {s}"
            );
        }
    }
}

// ============================================================================
// HTTP authority parity (CAD-1420). The availability gate is retired, so the
// flag is no longer a refusal expectation. The enabled-path tests above arm
// `HOSTED_CADENCE_ASSISTANT_ENABLED` through the daemon refusal module's shared
// `AssistantFlagOn` guard. The flag-unset child probes run in ISOLATED CHILD
// PROCESSES: this binary respawned with the flag absent/false/garbage. They
// assert forged-signature and wrong-org refusals, not a flag result.
// ============================================================================

/// Child-side probe, `#[ignore]`d so it never runs in the main suite; the
/// parent driver respawns it with the flag absent / `"false"` / garbage. The
/// availability gate is retired (CAD-1420), so the flag value is not an
/// expectation. The HTTP AUTHORITY guards must refuse with the flag unset:
/// a forged-signature envelope (signed by another key) and a wrong-org
/// envelope are each refused 401 at `POST /api/assist/assist_state` with no
/// tracker effect. A valid member
/// envelope is the positive control and must be admitted (200).
#[test]
#[ignore]
fn assist_http_authority_refuses_with_flag_unset_child() {
    let rig = start(&["alice"]);
    let before = tree(&rig.pm);
    let other_key = Ed25519KeyPair::from_seed_unchecked(&[9; 32]).unwrap();
    let forged = {
        let now = crate::issue::time::now_epoch();
        let h = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(
                &json!({"alg":"EdDSA","typ":"agenticos-assistant-actor/1","kid":KID}),
            )
            .unwrap(),
        );
        let c = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "iss": rig.issuer, "aud": format!("http://{}", rig.public_host),
                "sub":"alice","actor":"users/alice","principal_kind":"user",
                "role_at_issue":"member","organization_id":ORG,
                "assistant_id":"assist_alice","scope":["assist.read"],
                "credential_id":"cred-alice","iat":now,"exp":now+120,"jti":"jti-forged-child"
            }))
            .unwrap(),
        );
        let signed = format!("{h}.{c}");
        format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(other_key.sign(signed.as_bytes()).as_ref())
        )
    };
    let (status, body) = rig.call("assist_state", &forged, &json!({}));
    assert_eq!(status, 401, "forged-signature envelope admitted: {body}");
    assert_eq!(tree(&rig.pm), before, "forged-signature call had an effect");

    let wrong_org = rig.envelope("alice", json!({}), json!({"organization_id": "other-org"}));
    let (status, body) = rig.call("assist_state", &wrong_org, &json!({}));
    assert_eq!(
        status, 401,
        "wrong-org envelope admitted with flag unset: {body}"
    );
    assert_eq!(
        tree(&rig.pm),
        before,
        "a refused HTTP call with the flag unset had an effect"
    );
    let good = rig.envelope("alice", json!({}), json!({}));
    let (status, body) = rig.call("assist_state", &good, &json!({}));
    assert_eq!(
        status, 200,
        "valid member envelope refused with flag unset: {body}"
    );
}

/// Parent driver: respawns THIS test binary with the flag absent / `"false"`
/// / garbage and runs the ignored child probe in each; the child's exit code
/// is the verdict. No in-process env mutation, so the enabled suite can never
/// observe a torn flag.
#[test]
fn assist_http_authority_refuses_any_flag() {
    for (label, flag) in [
        ("absent", None),
        ("false", Some("false")),
        ("garbage", Some("not-a-bool")),
    ] {
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "ui::team_assistants_http_refusal::assist_http_authority_refuses_with_flag_unset_child",
            "--exact",
            "--include-ignored",
            "--test-threads",
            "1",
            "--nocapture",
        ]);
        match flag {
            Some(v) => {
                cmd.env("HOSTED_CADENCE_ASSISTANT_ENABLED", v);
            }
            None => {
                cmd.env_remove("HOSTED_CADENCE_ASSISTANT_ENABLED");
            }
        }
        let out = cmd.output().expect("flag-off child never ran");
        assert!(
            out.status.success(),
            "flag {label}: child probe failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// CAD-1380 manual-stop parity (W6/R7) over HTTP: the same member send the
/// daemon `member_assist_send` guard refuses must not re-enable an operator-
/// stopped assistant. The member is live and authenticated with `assist.write`,
/// so the request reaches the launch guard (not a blind 403). The expected
/// receipt is HTTP 200 with the text queued and attention naming the operator
/// pause; the endpoint stays disabled, unlaunched and unstamped.
#[test]
fn assist_http_send_respects_manual_stop() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["erin"]);
    // Register the member's assistant agent row (lazy, first assist_state).
    let (status, body) = rig.call(
        "assist_state",
        &rig.envelope("erin", json!({}), json!({})),
        &json!({}),
    );
    assert_eq!(status, 200, "live member state read refused: {body}");
    let alias = body["assistant"]["alias"]
        .as_str()
        .expect("assist_state bound no alias")
        .to_string();

    // Trusted state of a native operator stop: enabled=false plus the
    // `stop_requested` marker `rpc_stop` writes. Fake non-Devin model on the
    // operator allowlist, so the old path reaches the launch.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let mut pm_yaml = std::fs::read_to_string(rig.pm.join("pm.yaml")).unwrap_or_default();
    pm_yaml.push_str(
        "\npi:\n  models:\n    allow:\n      - fake/c1380-safe-model\n    default:\n      worker: fake/c1380-safe-model\n",
    );
    std::fs::write(rig.pm.join("pm.yaml"), pm_yaml).unwrap();
    shared.store.set_enabled(&alias, false).unwrap();
    shared
        .store
        .event_public(&alias, "stop_requested", json!({}))
        .unwrap();

    let (status, body) = rig.call(
        "assist_send",
        &rig.envelope("erin", json!({}), json!({})),
        &json!({"text": "http manual stop text"}),
    );
    assert_eq!(
        status, 200,
        "a queued send was refused as a blind 403: {body}"
    );
    assert_eq!(body["queued"].as_bool(), Some(true), "not queued: {body}");
    let attention = body["attention"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    assert!(
        attention.contains("stop") && attention.contains("resume"),
        "the receipt does not name the operator pause: {body}"
    );
    assert!(
        !shared.store.agent(&alias).unwrap().enabled,
        "an HTTP send re-enabled a manually stopped assistant"
    );
    let held = shared.store.queued_head(&alias).unwrap();
    assert_eq!(
        held.map(|m| m.body).as_deref(),
        Some("http manual stop text"),
        "the member's text was not retained"
    );
}
