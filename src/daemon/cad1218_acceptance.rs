//! CAD-1218 independent acceptance check — written by the check author from
//! the ticket, not by the implementer (AGENTS.md "Gates and security work").
//! The implementer may remove the `#[ignore]` below and nothing else; the
//! cases, their order and their assertions are the contract.
//!
//! The outcome under test: the operator records a PR-head merge approval
//! from the board (`POST /api/approvals/approve`, body exactly
//! `{"repo","pr","head"}`) and revokes it from the same place
//! (`POST /api/approvals/<id>/revoke`, body `{"reason"}`). The board relays
//! the daemon's operator-connection verb `approval_record_shown`, which
//! re-reads the PR with the daemon's own boot-fixed `gh` and records the
//! same `audit:approvals` event `cadence audit approve` records.
//!
//! Every refused case starts from a request that differs from an accepted
//! one in exactly the property under test — the caller, the session, one
//! field, the head, or "it was sent before" — so only the guard refuses:
//!
//! | case | guard exercised |
//! |---|---|
//! | agent caller, no session | `operator::admit` → `board_caller` (`operator_only`) |
//! | agent caller presenting the operator's session | `board_caller` (`session_from_agent`) |
//! | unprovable / daemon-descended caller with a session | `board_caller` / `home::prove_operator_peer` |
//! | forged session key, forged cookie | `check_session` (no session → refused) |
//! | agent or unproven caller on the daemon verb directly | `Shared::operator_connection` |
//! | forged field (`source`, `recorded_via`, `action`, `id`, `delegated`, `by`) | the route's `deny_unknown_fields` body |
//! | repo of no registered project | the route's/verb's project-repo allowlist |
//! | forged head, moved head | the verb's live-head compare (`head_moved`) |
//! | replay while the approval stands, replay after revoke | the verb's "one record per shown head" rule |
//! | tailnet actor with an empty allowlist, a login not on it, a public-session actor | the verb's approver allowlist (`pm.yaml` `approvals.tailnet_logins`) |
//! | public AgenticOS `owner` session on the approve route | the route's `Caller::Operator`-only check (`approver_not_allowed`) |
//!
//! The approver allowlist (operator decision on CAD-1218, 2026-10-09):
//! `approval_record_shown` accepts `request_actor` exactly `operator (ui)`
//! (a loopback board session) or `<login> (tailscale)` whose `<login>` is
//! listed in the tracker's `pm.yaml`:
//!
//! ```yaml
//! approvals:
//!   tailnet_logins: [chris@example.com]
//! ```
//!
//! read fresh on every call, matched exactly; absent or empty means no
//! remote approvals. Anything else is refused with a message containing
//! "approval allowlist". A tailnet request cannot be produced in a test —
//! `tailnet_proof::prove` needs a client socket owned by tailscaled's
//! foreign uid, and the seam never yields the tailnet origin — so the
//! tailnet cases drive the verb with the actor string the board derives
//! (`proxied_actor`), and the board's relay of that actor is the
//! Spec/security reviewer's probe (see the design note).
//!
//! What `scripts/enqueue-reviewed` sees is read through the audit's own
//! reader (`audit::approval_check`, the code behind `cadence audit
//! approval`), never through the board's reply.
//!
//! Seam note: identities are asserted with the CAD-482 test seam, which
//! stands in for the `/proc` ancestry walk only (the walk has its own tests
//! in `peer.rs`). The session, Origin, body, allowlist, head and replay
//! guards run unmodified.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::test_seam::{scoped, Asserted, Seam};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde_json::{json, Value};

const REPO: &str = "acme/widgets";
const PR: u64 = 42;
const HEAD: &str = "1218aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MOVED: &str = "1218bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const FORGED: &str = "1218cccccccccccccccccccccccccccccccccccc";
const VERB: &str = "approval_record_shown";
const APPROVE: &str = "/api/approvals/approve";
/// The PRs the allowlist cases use, so each starts with no record.
const TAILNET_PR: u64 = 43;
const PUBLIC_PR: u64 = 44;
/// This board's public AgenticOS name and the fixture platform.
const PUBLIC_HOST: &str = "c1218.board.localhost";
const COMPANY: &str = "c1218-company";
const KEY_SEED: [u8; 32] = [18; 32];

struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

/// A session as the browser holds it: the HttpOnly cookie and the page key.
#[derive(Clone)]
struct Session {
    cookie: String,
    key: String,
}

struct Board {
    state: std::path::PathBuf,
    live_head: std::path::PathBuf,
    agent: ureq::Agent,
    base: String,
    host: String,
    token: String,
    issuer: String,
    _stop: Stop,
}

/// A fixture AgenticOS platform: serves the JWKS for [`KEY_SEED`] until
/// the fixture stops. Returns its issuer origin.
fn platform(stop: &mut Stop) -> String {
    let server = (3110..=3199)
        .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
        .expect("no free fixture port in 3110..3199 for the platform");
    let issuer = format!("http://{}", server.server_addr());
    let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
    let x = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let flag = stop.0.clone();
    stop.1.push(std::thread::spawn(move || {
        while !flag.load(Ordering::SeqCst) {
            let Ok(Some(request)) = server.recv_timeout(Duration::from_millis(100)) else {
                continue;
            };
            let body = json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": "c1218", "x": x}]});
            let response = tiny_http::Response::from_string(body.to_string()).with_header(
                tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
            );
            let _ = request.respond(response);
        }
    }));
    issuer
}

/// A fake `gh` the daemon is booted with: `pr view <n> …` answers an open
/// PR whose head is the content of `live-head` beside it, whatever repo is
/// named — so the project-repo allowlist, not `gh`, refuses a forged repo.
fn fake_gh(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
here=$(dirname "$0")
if [ "$1" = pr ] && [ "$2" = view ]; then
  head=$(cat "$here/live-head")
  printf '{"number":%s,"state":"OPEN","title":"CAD-1218: fixture","headRefOid":"%s","headRefName":"cadence/cad-1218-fixture","baseRefName":"main","author":{"login":"fixture-author"},"statusCheckRollup":[],"additions":1,"deletions":0,"changedFiles":1,"files":[{"path":"src/lib.rs"}],"autoMergeRequest":null,"url":"https://github.com/acme/widgets/pull/%s"}\n' "$3" "$head" "$3"
  exit 0
fi
echo "fake gh: no fixture for: $*" >&2
exit 1
"#,
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    gh
}

impl Board {
    fn start(root: &std::path::Path) -> Self {
        let state = root.to_path_buf();
        let pm_dir = state.join("pm");
        let pm = crate::issue::Pm::init(&pm_dir).unwrap();
        // One registered project whose checkout's origin is acme/widgets.
        let checkout = state.join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        for args in [
            &["init", "-q"][..],
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ][..],
        ] {
            let ok = std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed in the fixture checkout");
        }
        crate::issue::write::project_add(
            &pm,
            "widgets",
            "WID",
            &[checkout.to_str().unwrap().to_string()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let live_head = state.join("live-head");
        std::fs::write(&live_head, HEAD).unwrap();
        let gh = fake_gh(&state);

        let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
        let issuer = platform(&mut stop);
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            delivery_gh: Some(gh),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let daemon_state = state.clone();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "test daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let token = Seam::token_at(&state).unwrap();
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.0.clone()),
                startup: Some(startup),
                test_seam: true,
                public: Some(crate::ui::PublicBoard {
                    host: PUBLIC_HOST.into(),
                    issuer: issuer.clone(),
                    company: COMPANY.into(),
                    authorize_url: format!("{issuer}/authorize"),
                }),
                allow_hosts: vec![PUBLIC_HOST.into()],
                allow_origins: vec![format!("http://{PUBLIC_HOST}")],
                ..Default::default()
            };
            let (s, p) = (state.clone(), pm_dir.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &opts)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => {
                    stop.1.push(thread);
                    break;
                }
                Err(_) if port < 3199 => {
                    port += 1;
                    thread.join().unwrap();
                }
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
        }
        crate::operator_auth::ensure_secret(&state).unwrap();
        // A registered agent, so an asserted agent identity resolves to a
        // real registry row on the daemon (the seam never invents one).
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "agent_register",
                json!({"alias": "worker", "provider": "inbox", "endpoint_kind": "inbox"}),
            )
        })
        .unwrap();
        Self {
            state,
            live_head,
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into(),
            base: format!("http://127.0.0.1:{port}"),
            host: format!("cadence-{port}.localhost:{port}"),
            token,
            issuer,
            _stop: stop,
        }
    }

    /// A fresh operator session through the real `cadence ui login` exchange.
    fn session(&self) -> Session {
        let secret = crate::operator_auth::read_secret(&self.state).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &self.state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let response = self
            .agent
            .post(format!("{}/api/session", self.base))
            .header("Host", &self.host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{}", self.host))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let set_cookie = response.headers()["set-cookie"].to_str().unwrap();
        let cookie = set_cookie[..set_cookie.find(';').unwrap()].to_owned();
        let body: Value = response.into_body().read_json().unwrap();
        Session {
            cookie,
            key: body["session_key"].as_str().unwrap().to_string(),
        }
    }

    /// One board write, byte for byte what a browser tab sends: `who` is
    /// the asserted peer identity, `session` what the request presents.
    fn post(&self, who: &str, session: Option<&Session>, path: &str, body: &str) -> (u16, Value) {
        let mut request = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{}", self.host))
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &self.token)
            .header("Content-Type", "application/json");
        if let Some(s) = session {
            request = request
                .header("Cookie", &s.cookie)
                .header("X-Cadence-Session", &s.key);
        }
        let response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.into_body().read_json().unwrap_or(Value::Null),
        )
    }

    /// What `cadence audit approval` — and so `scripts/enqueue-reviewed` —
    /// reads for `head` of the fixture PR.
    fn audit(&self, head: &str) -> Value {
        self.audit_pr(PR, head)
    }

    fn audit_pr(&self, pr: u64, head: &str) -> Value {
        crate::audit::approval_check(&self.state, REPO, pr, head).0
    }

    /// The daemon verb as the board relays it, from a proven operator
    /// connection, attributed to `actor`.
    fn shown(&self, pr: u64, actor: &str) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &self.state,
                VERB,
                json!({"repo": REPO, "pr": pr, "head": HEAD, "request_actor": actor}),
            )
        })
    }

    /// A public (AgenticOS) session for a platform-verified `owner`,
    /// through the daemon's real assertion exchange.
    fn public_owner_session(&self) -> String {
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = crate::issue::time::now_epoch();
        let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "c1218"})),
            part(json!({
                "iss": self.issuer, "aud": PUBLIC_HOST, "sub": "usr_owner",
                "email": "owner@example.com", "name": "Platform Owner",
                "company": COMPANY, "role": "owner",
                "iat": now, "exp": now + 30, "jti": "c1218-owner"
            }))
        );
        let assertion = format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
        );
        let opened = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &self.state,
                "board_session_open",
                json!({"assertion": assertion}),
            )
        })
        .unwrap();
        opened["token"].as_str().unwrap().to_string()
    }

    /// A request on the public host, as the platform relay delivers it: a
    /// GET without `body`, a POST with one.
    fn public(&self, path: &str, token: &str, body: Option<&str>) -> (u16, Value) {
        // `__Host-aos-board-session`: the public cookie (contract §7).
        let cookie = format!("__Host-aos-board-session={token}");
        let url = format!("{}{path}", self.base);
        let response = match body {
            Some(body) => self
                .agent
                .post(url)
                .header("Host", PUBLIC_HOST)
                .header("X-Cadence-Board", "1")
                .header("Origin", format!("http://{PUBLIC_HOST}"))
                .header(crate::test_seam::AS_HEADER, "operator")
                .header(crate::test_seam::TOKEN_HEADER, &self.token)
                .header("Content-Type", "application/json")
                .header("Cookie", &cookie)
                .send(body.to_string()),
            None => self
                .agent
                .get(url)
                .header("Host", PUBLIC_HOST)
                .header(crate::test_seam::AS_HEADER, "operator")
                .header(crate::test_seam::TOKEN_HEADER, &self.token)
                .header("Cookie", &cookie)
                .call(),
        }
        .unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.into_body().read_json().unwrap_or(Value::Null),
        )
    }

    fn move_head(&self, head: &str) {
        std::fs::write(&self.live_head, head).unwrap();
    }
}

fn approve_body(head: &str) -> String {
    json!({"repo": REPO, "pr": PR, "head": head}).to_string()
}

/// A refusal that is a refusal: 4xx, and nothing recorded for any head.
#[track_caller]
fn refused(board: &Board, (status, body): (u16, Value), what: &str) {
    assert!(
        (400..500).contains(&status),
        "{what}: expected a refusal, got {status} {body}"
    );
    for head in [HEAD, MOVED, FORGED] {
        assert_eq!(
            board.audit(head)["state"],
            "missing",
            "{what}: a refused request recorded an approval for {head}: {body}"
        );
    }
}

#[test]
fn cad1218_board_pr_head_approval_refuses_agent_forged_head_and_replay() {
    let root = tempfile::Builder::new().prefix("c1218").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let good = approve_body(HEAD);

    // --- agent callers and unprovable processes (the caller is the only difference) ---
    let (status, body) = board.post("agent:worker", None, APPROVE, &good);
    assert_eq!(status, 403, "agent without a session: {body}");
    assert_eq!(
        body["check"], "operator_only",
        "agent without a session: {body}"
    );
    refused(&board, (status, body), "agent without a session");

    let (status, body) = board.post("unproven", Some(&operator), APPROVE, &good);
    assert_eq!(
        status, 403,
        "unproven caller with the operator's session: {body}"
    );
    assert!(
        ["caller_identity", "operator_proof"].contains(&body["check"].as_str().unwrap_or("")),
        "unproven / daemon-descended caller must fail the process proof: {body}"
    );
    refused(&board, (status, body), "unproven caller with a session");

    // The daemon verb the board relays refuses an agent and an unprovable
    // caller that skip the board and dial the socket — with the exact
    // params the board sends.
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let out = scoped(who.clone(), || {
            crate::client::rpc(
                &board.state,
                VERB,
                json!({"repo": REPO, "pr": PR, "head": HEAD, "request_actor": "operator (ui)"}),
            )
        });
        let text = out
            .expect_err("the daemon verb admitted a non-operator")
            .to_string();
        assert!(
            !text.contains("Unknown method"),
            "{VERB} does not exist, so this case proves nothing: {text}"
        );
        assert!(
            text.contains("operator action"),
            "{who:?} on {VERB} must be refused by the operator-connection guard: {text}"
        );
        refused(&board, (403, json!(text)), "direct daemon call");
    }

    // --- the approver allowlist (pm.yaml `approvals.tailnet_logins`) ---
    // Each tailnet case is the board's own relay of a proven tailnet login;
    // only the allowlist differs between the refusals and the record.
    let chris = "chris@example.com (tailscale)";
    let empty = board.shown(TAILNET_PR, chris);
    let text = empty
        .expect_err("an empty allowlist admitted a tailnet login")
        .to_string();
    assert!(
        text.contains("approval allowlist"),
        "no allowlist means no remote approvals: {text}"
    );
    assert_eq!(board.audit_pr(TAILNET_PR, HEAD)["state"], "missing");
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("approvals:\n  tailnet_logins:\n    - chris@example.com\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    for actor in [
        "mallory@example.com (tailscale)",
        "chris@example.com.evil (tailscale)",
        "Platform Owner <owner@example.com> (board)",
        "chris@example.com",
    ] {
        let text = board
            .shown(TAILNET_PR, actor)
            .expect_err("an actor off the allowlist recorded an approval")
            .to_string();
        assert!(
            text.contains("approval allowlist"),
            "'{actor}' must be refused by the approver allowlist: {text}"
        );
        assert_eq!(board.audit_pr(TAILNET_PR, HEAD)["state"], "missing");
    }
    let out = board
        .shown(TAILNET_PR, chris)
        .expect("an allowlisted tailnet login records");
    assert!(out["approval_id"].is_string(), "{out}");
    let seen = board.audit_pr(TAILNET_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    assert!(
        seen["source"].as_str().is_some_and(|s| s.contains(chris)),
        "the source names the tailnet login, marked (tailscale): {seen}"
    );

    // --- a public AgenticOS owner session: valid, admitted by the board's
    // OperatorOnly class, and still no approver ---
    let owner = board.public_owner_session();
    let (status, meta) = board.public("/api/meta", &owner, None);
    assert_eq!(status, 200, "{meta}");
    assert_eq!(meta["signed_in"], true, "the owner session is live: {meta}");
    let public_body = json!({"repo": REPO, "pr": PUBLIC_PR, "head": HEAD}).to_string();
    let (status, body) = board.public(APPROVE, &owner, Some(&public_body));
    assert_eq!(status, 403, "a public owner session cannot approve: {body}");
    assert_eq!(
        body["check"], "approver_not_allowed",
        "refused by the approve route's Caller::Operator allowlist, not earlier: {body}"
    );
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "missing");

    // --- forged sessions ---
    let forged_key = Session {
        cookie: operator.cookie.clone(),
        key: "forged-session-key".into(),
    };
    refused(
        &board,
        board.post("operator", Some(&forged_key), APPROVE, &good),
        "the operator's cookie with a forged page key",
    );
    let forged_cookie = Session {
        cookie: format!("{}x", operator.cookie),
        key: operator.key.clone(),
    };
    refused(
        &board,
        board.post("operator", Some(&forged_cookie), APPROVE, &good),
        "a forged cookie with the operator's page key",
    );

    // --- forged fields: the board derives every one of these itself ---
    for (field, value) in [
        ("source", json!("chris in chat")),
        ("recorded_via", json!("operator-connection")),
        ("action", json!("delegated-merge")),
        ("action", json!("scope")),
        ("id", json!("merge-pr42-chosen")),
        ("delegated", json!(true)),
        ("by", json!("operator")),
        ("request_actor", json!("someone else")),
    ] {
        let mut body: Value = serde_json::from_str(&good).unwrap();
        body[field] = value;
        refused(
            &board,
            board.post("operator", Some(&operator), APPROVE, &body.to_string()),
            &format!("forged field '{field}'"),
        );
    }
    // The allowlist of repos: a repo no registered project names, even
    // though `gh` would answer for it with the same open PR and head.
    refused(
        &board,
        board.post(
            "operator",
            Some(&operator),
            APPROVE,
            &json!({"repo": "acme/not-a-project", "pr": PR, "head": HEAD}).to_string(),
        ),
        "a repo of no registered project",
    );
    assert_eq!(
        crate::audit::approval_check(&board.state, "acme/not-a-project", PR, HEAD).0["state"],
        "missing"
    );

    // --- forged head and moved head: the PR's live head is the only thing that differs ---
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &approve_body(FORGED));
    assert_eq!(status, 409, "a head that is not the PR's head: {body}");
    assert_eq!(body["code"], "head_moved", "a forged head: {body}");
    refused(&board, (status, body), "a forged head");

    board.move_head(MOVED);
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &good);
    assert_eq!(status, 409, "the head the drawer showed has moved: {body}");
    assert_eq!(body["code"], "head_moved", "a moved head: {body}");
    refused(&board, (status, body), "a moved head");
    board.move_head(HEAD);

    // --- positive control: the identical request, from the operator, records ---
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &good);
    assert_eq!(
        status, 200,
        "the operator's approval of the shown head: {body}"
    );
    let id = body["approval_id"]
        .as_str()
        .expect("the reply names the approval id")
        .to_string();
    let seen = board.audit(HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], id.as_str(), "{seen}");
    // Exactly what a CLI record carries, so enqueue-reviewed accepts it.
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    assert!(
        seen["source"]
            .as_str()
            .is_some_and(|s| s.contains("operator (ui)")),
        "the source names the board session's operator, derived by the board: {seen}"
    );
    for other in [MOVED, FORGED] {
        assert_eq!(board.audit(other)["state"], "missing");
    }

    // --- replay while the approval stands ---
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &good);
    assert!(
        (400..500).contains(&status),
        "a replay of the approval is refused, not re-applied: {status} {body}"
    );
    let seen = board.audit(HEAD);
    assert_eq!(seen["state"], "in-force");
    assert_eq!(
        seen["approval_id"],
        id.as_str(),
        "a replay minted a new record: {seen}"
    );

    // --- revoke from the same place ---
    let revoke = format!("/api/approvals/{id}/revoke");
    let reason = json!({"reason": "re-review"}).to_string();
    let (status, body) = board.post("agent:worker", None, &revoke, &reason);
    assert_eq!(status, 403, "an agent cannot revoke: {body}");
    assert_eq!(board.audit(HEAD)["state"], "in-force");
    let (status, body) = board.post("operator", Some(&operator), &revoke, &reason);
    assert_eq!(status, 200, "the operator revokes from the board: {body}");
    assert_eq!(board.audit(HEAD)["state"], "revoked");

    // --- replay after the revoke: the old request must not resurrect it ---
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &good);
    assert!(
        (400..500).contains(&status),
        "a replay after revoke is refused: {status} {body}"
    );
    let seen = board.audit(HEAD);
    assert_eq!(
        seen["state"], "revoked",
        "a replayed approval re-recorded a revoked head: {seen}"
    );

    // --- last, because it revokes the session: an agent presenting the
    // operator's own session is refused and the session dies ---
    let other = approve_body(MOVED);
    board.move_head(MOVED);
    let (status, body) = board.post("agent:worker", Some(&operator), APPROVE, &other);
    assert_eq!(
        status, 403,
        "an agent presenting the operator's session: {body}"
    );
    assert_eq!(body["check"], "session_from_agent", "{body}");
    assert_eq!(board.audit(MOVED)["state"], "missing");
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &other);
    assert_eq!(status, 403, "the stolen session is revoked: {body}");
    assert_eq!(board.audit(MOVED)["state"], "missing");
}
