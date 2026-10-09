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
//! | (round 5) public AgenticOS `owner` approves; `member` never | see `cad1218_platform_owner_is_an_approver_and_member_never` |
//!
//! Round 2 adds two sibling tests below (each ignored until built): the
//! drawer's approval-state read
//! (`cad1218_board_approval_state_read_is_operator_only_and_leaks_nothing`)
//! and the shared approver rule on Publish and revoke plus non-OPEN PRs
//! (`cad1218_approver_rule_covers_publish_revoke_and_closed_prs`). Their
//! contracts are in the comment blocks above each.
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
/// The delivery-loop rows the Publish cases decide (round 2).
const PUBLISH_ISSUE: &str = "WID-1";
const PUBLISH_PR: u64 = 50;
const PUBLISH_ISSUE_2: &str = "WID-2";
const PUBLISH_PR_2: u64 = 51;

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

/// A fake `gh` the daemon is booted with: `pr view <n> …` answers a PR
/// whose head is the content of `live-head` beside it, whatever repo is
/// named — so the project-repo allowlist, not `gh`, refuses a forged repo.
/// Its state is `pr-state-<n>` beside it when that file exists, else
/// `OPEN`. `pr merge …` succeeds and appends its argv to `merges`, so a
/// refused Publish can be shown to have enqueued nothing.
fn fake_gh(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let gh = dir.join("gh");
    std::fs::write(
        &gh,
        r#"#!/bin/sh
here=$(dirname "$0")
if [ "$1" = pr ] && [ "$2" = merge ]; then
  echo "$*" >> "$here/merges"
  exit 0
fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  head=$(cat "$here/live-head")
  state=OPEN
  if [ -f "$here/pr-state-$3" ]; then state=$(cat "$here/pr-state-$3"); fi
  printf '{"number":%s,"state":"%s","title":"CAD-1218: fixture","headRefOid":"%s","headRefName":"cadence/cad-1218-fixture","baseRefName":"main","author":{"login":"fixture-author"},"statusCheckRollup":[],"additions":1,"deletions":0,"changedFiles":1,"files":[{"path":"src/lib.rs"}],"autoMergeRequest":null,"url":"https://github.com/acme/widgets/pull/%s"}\n' "$3" "$state" "$head" "$3"
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
        // Two merge decisions waiting on the operator (CAD-431 Publish):
        // PASS on the live head, so each is merge-ready once read.
        let mut loop_records = std::collections::BTreeMap::new();
        for (issue, pr) in [(PUBLISH_ISSUE, PUBLISH_PR), (PUBLISH_ISSUE_2, PUBLISH_PR_2)] {
            let mut rec = crate::delivery::Record::new(issue, "widgets", "fixture-worker", 1);
            rec.state = crate::delivery::State::Passed;
            rec.pr = Some(format!("https://github.com/{REPO}/pull/{pr}"));
            rec.head = Some(HEAD.into());
            rec.verdict = Some(crate::delivery::VerdictRec {
                verdict: "pass".into(),
                sha: HEAD.into(),
                reviewer: "fixture-reviewer".into(),
                summary: "fixture pass".into(),
                report: format!("{issue}/reports/fixture.md"),
                at: 1,
            });
            loop_records.insert(issue.to_string(), rec);
        }
        crate::delivery::save(&state, &loop_records).unwrap();

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

    /// One board read, as a tab sends it: no body, no Origin.
    fn get(&self, who: &str, session: Option<&Session>, path: &str) -> (u16, Value) {
        let mut request = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &self.token);
        if let Some(s) = session {
            request = request
                .header("Cookie", &s.cookie)
                .header("X-Cadence-Session", &s.key);
        }
        let response = request.call().unwrap();
        let status = response.status().as_u16();
        (
            status,
            response.into_body().read_json().unwrap_or(Value::Null),
        )
    }

    /// Every `audit:approvals` event the store holds — a read must leave
    /// this exactly as it found it.
    fn approval_events(&self) -> i64 {
        let db = rusqlite::Connection::open_with_flags(
            self.state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.query_row(
            "SELECT count(*) FROM events WHERE alias = 'audit:approvals'",
            [],
            |r| r.get(0),
        )
        .unwrap()
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

    /// A public (AgenticOS) session for a platform-verified `owner` or
    /// `member` (`role`), through the daemon's real assertion exchange. The
    /// board names it `Platform Owner <owner@example.com> (board)` /
    /// `Platform Member <member@example.com> (board)`.
    fn public_session(&self, role: &str) -> String {
        let name = if role == "owner" {
            "Platform Owner"
        } else {
            "Platform Member"
        };
        let key = Ed25519KeyPair::from_seed_unchecked(&KEY_SEED).unwrap();
        let now = crate::issue::time::now_epoch();
        let part = |v: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&v).unwrap());
        let signed = format!(
            "{}.{}",
            part(json!({"alg": "EdDSA", "typ": "JWT", "kid": "c1218"})),
            part(json!({
                "iss": self.issuer, "aud": PUBLIC_HOST, "sub": format!("usr_{role}"),
                "email": format!("{role}@example.com"), "name": name,
                "company": COMPANY, "role": role,
                "iat": now, "exp": now + 30, "jti": format!("c1218-{role}")
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
    /// GET without `body`, a POST with one. Each carries a forged
    /// `Tailscale-User-Login: mallory@example.com` — a public session must
    /// never be attributed to a tailnet login.
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
                .header("Tailscale-User-Login", "mallory@example.com")
                .header("Cookie", &cookie)
                .send(body.to_string()),
            None => self
                .agent
                .get(url)
                .header("Host", PUBLIC_HOST)
                .header(crate::test_seam::AS_HEADER, "operator")
                .header(crate::test_seam::TOKEN_HEADER, &self.token)
                .header("Tailscale-User-Login", "mallory@example.com")
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

    // (Round 5: a platform-verified owner is an approver — operator
    // decision on CAD-1218 — so the owner's approval is pinned in
    // `cad1218_platform_owner_is_an_approver_and_member_never`.)

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

// ---------------------------------------------------------------------
// Round 2 (Browser QA F1 on PR #899): the drawer must read whether the
// head it shows already has an approval, so a standing approval offers
// Revoke instead of a second Approve. That read is a new board rule; this
// is its independent refusal check (the implementer may not edit it).
//
// Contract:
// - `GET /api/approvals/state?repo=<owner/name>&pr=<n>&head=<40-hex>` —
//   exactly these three keys, each once; an unknown or repeated key, or a
//   head that is not the full lowercase 40-hex SHA, is 400 before any
//   lookup.
// - Admission: `operator::admit_operator_read` (session, page key, and the
//   HTTP-peer operator proof), then the approvers only: `Caller::Operator`,
//   and (round 5) a platform-verified `owner` session; a `member` is
//   refused by admission itself (`member_role`).
// - The board relays the daemon verb `approval_state`
//   `{repo, pr, head, request_actor}`: `operator_connection`, the repo of
//   a registered project (else refused — no state for it), and the same
//   approver allowlist as `approval_record_shown` (a `pm.yaml` that does
//   not parse refuses). It answers from the audit reader
//   (`audit::approval_check`).
// - 200 body is exactly `{"state": "in-force"|"revoked"|"missing"}` plus
//   `"approval_id"` for in-force and revoked — no source, no logins, no
//   other heads. The read writes nothing.
// ---------------------------------------------------------------------

const STATE_VERB: &str = "approval_state";
const CHRIS_LOGIN: &str = "chris@example.com (tailscale)";

fn state_path(repo: &str, pr: u64, head: &str) -> String {
    format!("/api/approvals/state?repo={repo}&pr={pr}&head={head}")
}

/// A 200 state answer, and nothing in it but the state and its id.
#[track_caller]
fn state_is(board: &Board, (status, body): (u16, Value), want: &str, id: Option<&str>) {
    assert_eq!(status, 200, "the operator's read: {body}");
    let keys: Vec<&String> = body.as_object().expect("a JSON object").keys().collect();
    // Round 3 adds `board_revocable` (a boolean, in-force only); nothing
    // else may appear.
    let allowed = ["state", "approval_id", "board_revocable"];
    assert!(
        keys.iter().all(|k| allowed.contains(&k.as_str())),
        "the read answers only state, approval_id and board_revocable: {body}"
    );
    assert_eq!(body["state"], want, "{body}");
    match id {
        Some(id) => assert_eq!(body["approval_id"], id, "{body}"),
        None => assert!(body.get("approval_id").is_none(), "{body}"),
    }
    let _ = board;
}

/// A refused read: 4xx, no state, and no approval id anywhere in it.
#[track_caller]
fn read_refused((status, body): (u16, Value), ids: &[&str], what: &str) {
    assert!(
        (400..500).contains(&status),
        "{what}: expected a refusal, got {status} {body}"
    );
    assert!(
        body.get("state").is_none(),
        "{what}: a refused read answered a state: {body}"
    );
    let text = body.to_string();
    for id in ids {
        assert!(
            !text.contains(id),
            "{what}: the refusal leaked {id}: {body}"
        );
    }
}

#[test]
fn cad1218_board_approval_state_read_is_operator_only_and_leaks_nothing() {
    let root = tempfile::Builder::new().prefix("c1218s").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let read = state_path(REPO, PR, HEAD);

    // --- positive control, part 1: missing, then the board records ---
    state_is(
        &board,
        board.get("operator", Some(&operator), &read),
        "missing",
        None,
    );
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &approve_body(HEAD));
    assert_eq!(status, 200, "the approval the read must find: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    state_is(
        &board,
        board.get("operator", Some(&operator), &read),
        "in-force",
        Some(&id),
    );
    // A record for a repo of no registered project, made on the CLI path,
    // so the board's project allowlist is the only thing hiding it.
    let foreign = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &board.state,
            "approval_record",
            json!({"repo": "acme/not-a-project", "pr": PR, "head": HEAD,
                   "source": "fixture operator", "action": "merge"}),
        )
    })
    .unwrap();
    let foreign_id = foreign["approval_id"].as_str().unwrap().to_string();
    let ids = [id.as_str(), foreign_id.as_str()];
    let events = board.approval_events();

    // --- the read is per head: other heads and PRs report nothing ---
    for other in [state_path(REPO, PR, FORGED), state_path(REPO, PR, MOVED)] {
        state_is(
            &board,
            board.get("operator", Some(&operator), &other),
            "missing",
            None,
        );
    }
    state_is(
        &board,
        board.get("operator", Some(&operator), &state_path(REPO, PR + 1, HEAD)),
        "missing",
        None,
    );

    // --- callers (the caller is the only difference) ---
    let (status, body) = board.get("agent:worker", None, &read);
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["check"], "operator_only", "{body}");
    read_refused((status, body), &ids, "agent without a session");
    let (status, body) = board.get("unproven", Some(&operator), &read);
    assert_eq!(status, 403, "{body}");
    assert!(
        ["caller_identity", "operator_proof"].contains(&body["check"].as_str().unwrap_or("")),
        "unproven / daemon-descended caller: {body}"
    );
    read_refused((status, body), &ids, "unproven caller with a session");
    let forged_key = Session {
        cookie: operator.cookie.clone(),
        key: "forged-session-key".into(),
    };
    read_refused(
        board.get("operator", Some(&forged_key), &read),
        &ids,
        "the operator's cookie with a forged page key",
    );
    let forged_cookie = Session {
        cookie: format!("{}x", operator.cookie),
        key: operator.key.clone(),
    };
    read_refused(
        board.get("operator", Some(&forged_cookie), &read),
        &ids,
        "a forged cookie with the operator's page key",
    );
    // --- the request shape ---
    read_refused(
        board.get(
            "operator",
            Some(&operator),
            &state_path("acme/not-a-project", PR, HEAD),
        ),
        &ids,
        "a repo of no registered project",
    );
    for (path, what) in [
        (format!("{read}&source=x"), "an unknown key"),
        (format!("{read}&id={id}"), "an id key"),
        (format!("{read}&head={FORGED}"), "a repeated head"),
        (format!("{read}&repo=acme/not-a-project"), "a repeated repo"),
        (state_path(REPO, PR, &HEAD[..12]), "a short head"),
        (
            state_path(REPO, PR, &HEAD.to_ascii_uppercase()),
            "an uppercase head",
        ),
        (
            format!("/api/approvals/state?repo={REPO}&pr={PR}"),
            "no head",
        ),
    ] {
        let (status, body) = board.get("operator", Some(&operator), &path);
        assert_eq!(status, 400, "{what}: {body}");
        read_refused((status, body), &ids, what);
    }

    // --- the daemon verb the board relays refuses a direct dial ---
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let text = scoped(who.clone(), || {
            crate::client::rpc(
                &board.state,
                STATE_VERB,
                json!({"repo": REPO, "pr": PR, "head": HEAD, "request_actor": "operator (ui)"}),
            )
        })
        .expect_err("the state verb answered a non-operator")
        .to_string();
        assert!(
            !text.contains("Unknown method"),
            "{STATE_VERB} does not exist, so this case proves nothing: {text}"
        );
        assert!(
            text.contains("operator action"),
            "{who:?} on {STATE_VERB}: {text}"
        );
        assert!(
            !text.contains(&id),
            "{who:?}: the refusal leaked the id: {text}"
        );
    }

    // --- round 4: the approver rule guards the read too. The head has a
    // real in-force record; only the relayed actor or the allowlist
    // differs from the accepted read below ---
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let before = std::fs::read_to_string(&pm_yaml).unwrap();
    let as_actor = |actor: &str| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(
                &board.state,
                STATE_VERB,
                json!({"repo": REPO, "pr": PR, "head": HEAD, "request_actor": actor}),
            )
        })
    };
    let off_list = |actor: &str, what: &str| {
        let text = as_actor(actor)
            .expect_err("the state read answered an actor off the allowlist")
            .to_string();
        assert!(
            text.contains("approval allowlist"),
            "{what}: refused by the approver rule: {text}"
        );
        for leaked in ids.iter().chain(["in-force", "revoked"].iter()) {
            assert!(
                !text.contains(leaked),
                "{what}: the refusal leaked {leaked}: {text}"
            );
        }
    };
    off_list(
        CHRIS_LOGIN,
        "an allowlisted-shape login with an empty allowlist",
    );
    std::fs::write(
        &pm_yaml,
        format!("{before}approvals:\n  tailnet_logins:\n    - chris@example.com\n"),
    )
    .unwrap();
    off_list(
        "mallory@example.com (tailscale)",
        "a login off the allowlist",
    );
    let seen = as_actor(CHRIS_LOGIN).expect("the allowlisted login reads");
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], id.as_str(), "{seen}");
    std::fs::write(&pm_yaml, &before).unwrap();

    // --- a pm.yaml that does not parse makes the read refuse, not answer ---
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let good_yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    std::fs::write(&pm_yaml, "approvals: [\n  tailnet_logins: {\n").unwrap();
    // Fail closed: any non-2xx (the server's own config is broken, so a
    // 5xx is honest), with no state and no id in it.
    let (status, body) = board.get("operator", Some(&operator), &read);
    assert!(
        !(200..300).contains(&status),
        "a malformed pm.yaml must not answer the read: {status} {body}"
    );
    assert!(body.get("state").is_none(), "{body}");
    for leaked in ids {
        assert!(!body.to_string().contains(leaked), "{body}");
    }
    std::fs::write(&pm_yaml, &good_yaml).unwrap();

    // --- no read wrote anything ---
    assert_eq!(
        board.approval_events(),
        events,
        "a read wrote to the approval stream"
    );
    assert_eq!(board.audit(HEAD)["state"], "in-force");
    assert_eq!(board.audit(FORGED)["state"], "missing");

    // --- positive control, part 2: revoke, and the read follows ---
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &format!("/api/approvals/{id}/revoke"),
        &json!({"reason": "re-review"}).to_string(),
    );
    assert_eq!(status, 200, "{body}");
    state_is(
        &board,
        board.get("operator", Some(&operator), &read),
        "revoked",
        Some(&id),
    );

    // --- last, because it revokes that session: an agent presenting an
    // operator session reads nothing and kills the session ---
    let stolen = board.session();
    let (status, body) = board.get("agent:worker", Some(&stolen), &read);
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["check"], "session_from_agent", "{body}");
    read_refused((status, body), &ids, "an agent with an operator session");
    read_refused(
        board.get("operator", Some(&stolen), &read),
        &ids,
        "the stolen session after its revoke",
    );
}

// ---------------------------------------------------------------------
// Round 2, Spec/security finding on PR #899 and the operator's decision
// (CAD-1218 comment, 2026-10-09): the approver rule is one shared daemon
// function, `approver_source` in `src/daemon/approvals_rpc.rs`, applied
// before anything is recorded by `approval_record_shown`,
// `rpc_delivery_approve` (the drawer's Publish) and the board's revoke.
// Given the `pm.yaml` config and the relayed `request_actor`, it admits
// exactly `operator (ui)` or a `<login> (tailscale)` listed in
// `approvals.tailnet_logins`, refuses everything else with a message
// containing "approval allowlist", and returns the record's source,
// `"<actor> via board"`. `delivery_approve` without a `request_actor`
// (the CLI's `cadence delivery merge`) keeps its CLI source `operator`.
//
// The board's revoke relays `approval_revoke_shown {id, reason,
// request_actor}` (operator connection, the approver rule). It reaches
// only what a board path recorded: an approval whose action is `merge`,
// `recorded_via` is `operator-connection` and source ends " via board".
// That is the stricter choice: delegated (`delegated-merge`), scope, CLI
// and any other action's records are refused, and the operator revokes
// them with `cadence audit revoke`.
//
// `approval_record_shown` refuses a PR that is not OPEN.
// ---------------------------------------------------------------------

const REVOKE_VERB: &str = "approval_revoke_shown";
const CHRIS: &str = "chris@example.com (tailscale)";
const MALLORY: &str = "mallory@example.com (tailscale)";

fn merges(board: &Board) -> String {
    std::fs::read_to_string(board.state.join("merges")).unwrap_or_default()
}

fn delivery_state(board: &Board, issue: &str) -> String {
    crate::delivery::load(&board.state).unwrap()[issue]
        .state
        .as_str()
        .to_string()
}

#[test]
fn cad1218_approver_rule_covers_publish_revoke_and_closed_prs() {
    let root = tempfile::Builder::new().prefix("c1218p").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("approvals:\n  tailnet_logins:\n    - chris@example.com\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    let publish = format!("/api/delivery/{PUBLISH_ISSUE}/merge");
    let publish_body = json!({"sha": HEAD}).to_string();
    let rpc = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&board.state, method, params)
        })
    };

    // --- (1) Publish: an off-list tailnet login, relayed exactly as the
    // board relays a proven tailnet caller, on a merge-ready row ---
    for actor in [MALLORY, "chris@example.com"] {
        let text = rpc(
            "delivery_approve",
            json!({"issue": PUBLISH_ISSUE, "sha": HEAD, "request_actor": actor}),
        )
        .expect_err("Publish recorded an approval for an actor off the allowlist")
        .to_string();
        assert!(
            text.contains("approval allowlist"),
            "'{actor}' on Publish must be refused by the approver rule: {text}"
        );
        assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
        assert_eq!(merges(&board), "", "a refused Publish enqueued a merge");
        assert_eq!(delivery_state(&board, PUBLISH_ISSUE), "passed");
    }
    // Positive controls: the loopback operator on the board, and an
    // allowlisted tailnet login through the same verb.
    let (status, body) = board.post("operator", Some(&operator), &publish, &publish_body);
    assert_eq!(status, 200, "the loopback operator still Publishes: {body}");
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    assert!(
        seen["source"]
            .as_str()
            .is_some_and(|s| s.contains("operator (ui)")),
        "{seen}"
    );
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));
    let out = rpc(
        "delivery_approve",
        json!({"issue": PUBLISH_ISSUE_2, "sha": HEAD, "request_actor": CHRIS}),
    )
    .expect("an allowlisted tailnet login still Publishes");
    assert!(out["approval_id"].is_string(), "{out}");
    let seen = board.audit_pr(PUBLISH_PR_2, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert!(
        seen["source"].as_str().is_some_and(|s| s.contains(CHRIS)),
        "{seen}"
    );

    // --- (2) Revoke: the approver rule, and only board-recorded ids ---
    let chris_id = rpc(
        VERB,
        json!({"repo": REPO, "pr": TAILNET_PR, "head": HEAD, "request_actor": CHRIS}),
    )
    .unwrap()["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let text = scoped(who.clone(), || {
            crate::client::rpc(
                &board.state,
                REVOKE_VERB,
                json!({"id": chris_id, "reason": "r", "request_actor": "operator (ui)"}),
            )
        })
        .expect_err("the revoke verb admitted a non-operator")
        .to_string();
        assert!(
            !text.contains("Unknown method"),
            "{REVOKE_VERB} does not exist, so this case proves nothing: {text}"
        );
        assert!(text.contains("operator action"), "{who:?}: {text}");
    }
    let text = rpc(
        REVOKE_VERB,
        json!({"id": chris_id, "reason": "not mine", "request_actor": MALLORY}),
    )
    .expect_err("an off-list tailnet login revoked chris's approval")
    .to_string();
    assert!(text.contains("approval allowlist"), "{text}");
    assert_eq!(board.audit_pr(TAILNET_PR, HEAD)["state"], "in-force");
    // Records no board path wrote: a CLI merge approval, a CLI record of
    // another action whose source imitates the board's, and a ticket
    // scope approval. Each is in force and stays so.
    let cli_merge = rpc(
        "approval_record",
        json!({"repo": REPO, "pr": 70, "head": HEAD, "source": "chris in chat"}),
    )
    .unwrap()["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let cli_other = rpc(
        "approval_record",
        json!({"repo": REPO, "pr": 71, "head": HEAD, "action": "deploy",
               "source": "operator (ui) via board"}),
    )
    .unwrap()["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pm = crate::issue::Pm::at(&board.state.join("pm")).unwrap();
    let scoped_issue = crate::issue::write::new_issue(
        &pm,
        &pm.dir,
        Some("widgets"),
        "scope fixture",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        None,
        "fixture-author",
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let scope_id = rpc(
        "approval_scope",
        json!({"issue": scoped_issue, "source": "operator in chat"}),
    )
    .unwrap()["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let events = board.approval_events();
    for (id, what) in [
        (&cli_merge, "a CLI merge approval"),
        (&cli_other, "another action's record"),
        (&scope_id, "a scope approval"),
    ] {
        let (status, body) = board.post(
            "operator",
            Some(&operator),
            &format!("/api/approvals/{id}/revoke"),
            &json!({"reason": "board reach"}).to_string(),
        );
        assert!(
            (400..500).contains(&status),
            "the board revoked {what} ({id}): {status} {body}"
        );
    }
    assert_eq!(board.audit_pr(70, HEAD)["state"], "in-force");
    let (scopes, _) = crate::audit::scope_check(&board.state, &scoped_issue);
    assert_eq!(scopes["approvals"][0]["revoked"], false, "{scopes}");
    // `cli_other` is not a merge, so the audit reader never shows it: the
    // approval stream gained nothing, so no revoke landed on any of them.
    assert_eq!(board.approval_events(), events, "a refused revoke wrote");
    // Positive control: the allowlisted login revokes a board record.
    rpc(
        REVOKE_VERB,
        json!({"id": chris_id, "reason": "re-review", "request_actor": CHRIS}),
    )
    .expect("an allowlisted login revokes a board-recorded approval");
    assert_eq!(board.audit_pr(TAILNET_PR, HEAD)["state"], "revoked");

    // --- (3) A PR that is not open has no head to approve ---
    for (pr, state) in [(80u64, "CLOSED"), (81, "MERGED")] {
        std::fs::write(board.state.join(format!("pr-state-{pr}")), state).unwrap();
        let body = json!({"repo": REPO, "pr": pr, "head": HEAD}).to_string();
        let (status, reply) = board.post("operator", Some(&operator), APPROVE, &body);
        assert!(
            (400..500).contains(&status),
            "a {state} PR must not be approved: {status} {reply}"
        );
        assert_eq!(board.audit_pr(pr, HEAD)["state"], "missing");
        // The identical request once the PR is open again records.
        std::fs::write(board.state.join(format!("pr-state-{pr}")), "OPEN").unwrap();
        let (status, reply) = board.post("operator", Some(&operator), APPROVE, &body);
        assert_eq!(status, 200, "the open PR's head approves: {reply}");
    }
}

// ---------------------------------------------------------------------
// Round 3 (Browser QA F2 on PR #899): the drawer offers "Take my approval
// back" only where the board's revoke would accept it.
//
// Contract:
// - The state read's 200 body gains `board_revocable` (a JSON boolean)
//   when, and only when, `state` is `in-force`; it is omitted for
//   `missing` and `revoked`. It describes the record whose
//   `approval_id` the read returns.
// - It is the board-revoke target predicate, one pure function in
//   `src/store/events.rs`: `board_revocable(record: &Value) -> bool` —
//   action `merge`, `recorded_via` `operator-connection`, source ending
//   " via board" — called by both `Store::revoke_board_approval` and
//   `approval_state`. So `board_revocable == true` ⇔ the board revoke is
//   accepted; the check proves it both ways on every in-force kind.
// - The approver rule's refusal is machine-readable: `approver_source`
//   fails with the structured code `approver_not_allowed` (as
//   `head_moved` is), and every board relay of an approval verb
//   (approve, revoke, state, Publish) answers it as
//   `403 {"check": "approver_not_allowed"}`. The Publish route's other
//   refusals keep their existing `check` codes (`operator_only`,
//   `caller_identity` / `operator_proof`).
// ---------------------------------------------------------------------

/// The read for `pr`'s HEAD as the loopback operator sees it.
fn read_state(board: &Board, operator: &Session, pr: u64) -> Value {
    let (status, body) = board.get("operator", Some(operator), &state_path(REPO, pr, HEAD));
    assert_eq!(status, 200, "the operator's read of PR {pr}: {body}");
    body
}

/// The board revoke of `id`: whether it was accepted, and the reply.
fn board_revoke(board: &Board, operator: &Session, id: &str) -> (bool, u16, Value) {
    let (status, body) = board.post(
        "operator",
        Some(operator),
        &format!("/api/approvals/{id}/revoke"),
        &json!({"reason": "drawer revoke"}).to_string(),
    );
    ((200..300).contains(&status), status, body)
}

#[test]
#[ignore = "CAD-1218: enabled by the implementation"]
fn cad1218_board_revocable_is_the_board_revoke_predicate() {
    let root = tempfile::Builder::new().prefix("c1218v").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let pm_yaml = board.state.join("pm").join("pm.yaml");
    let mut yaml = std::fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("approvals:\n  tailnet_logins:\n    - chris@example.com\n");
    std::fs::write(&pm_yaml, yaml).unwrap();
    let rpc = |method: &str, params: Value| {
        scoped(Asserted::Operator, || {
            crate::client::rpc(&board.state, method, params)
        })
    };
    let publish = format!("/api/delivery/{PUBLISH_ISSUE}/merge");
    let publish_body = json!({"sha": HEAD}).to_string();

    // --- the approver rule's refusal carries a stable code ---
    // Every verb the board relays, with the actor the board would relay for
    // an off-list tailnet login; each request is otherwise accepted.
    for (method, params) in [
        (
            "delivery_approve",
            json!({"issue": PUBLISH_ISSUE, "sha": HEAD, "request_actor": MALLORY}),
        ),
        (
            VERB,
            json!({"repo": REPO, "pr": 95, "head": HEAD, "request_actor": MALLORY}),
        ),
        (
            STATE_VERB,
            json!({"repo": REPO, "pr": 95, "head": HEAD, "request_actor": MALLORY}),
        ),
        (
            REVOKE_VERB,
            json!({"id": "merge-pr95-unknown", "reason": "r", "request_actor": MALLORY}),
        ),
    ] {
        let err = rpc(method, params).expect_err("an off-list login passed the approver rule");
        assert_eq!(
            err.code(),
            Some("approver_not_allowed"),
            "{method}: the approver refusal is machine-readable: {err}"
        );
    }
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "");
    // Publish over HTTP: each refusal names its check.
    for (who, session, want) in [
        ("agent:worker", None, &["operator_only"][..]),
        (
            "unproven",
            Some(&operator),
            &["caller_identity", "operator_proof"][..],
        ),
    ] {
        let (status, body) = board.post(who, session, &publish, &publish_body);
        assert_eq!(status, 403, "{who} on Publish: {body}");
        assert!(
            want.contains(&body["check"].as_str().unwrap_or("")),
            "{who} on Publish names its check {want:?}: {body}"
        );
    }
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "", "a refused Publish enqueued a merge");

    // --- board_revocable is the revoke predicate, proven both ways ---
    // Missing: no flag.
    let missing = read_state(&board, &operator, 93);
    assert_eq!(missing["state"], "missing", "{missing}");
    assert!(missing.get("board_revocable").is_none(), "{missing}");

    // Recorded on the board: Approve, Publish, and an allowlisted tailnet
    // login through the verb — each true, and each board revoke accepted.
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &approve_body(HEAD));
    assert_eq!(status, 200, "{body}");
    let (status, body) = board.post("operator", Some(&operator), &publish, &publish_body);
    assert_eq!(status, 200, "{body}");
    rpc(
        VERB,
        json!({"repo": REPO, "pr": TAILNET_PR, "head": HEAD, "request_actor": CHRIS}),
    )
    .unwrap();
    // Recorded on the terminal path (`cadence audit approve` sends
    // `approval_record`): false, and the board revoke refused.
    rpc(
        "approval_record",
        json!({"repo": REPO, "pr": 90, "head": HEAD, "source": "chris in chat"}),
    )
    .unwrap();
    // The predicate is the record's fields, so a terminal record that
    // carries the board's source shape is revocable — and is revoked.
    rpc(
        "approval_record",
        json!({"repo": REPO, "pr": 91, "head": HEAD, "source": "operator (ui) via board"}),
    )
    .unwrap();
    for (pr, want, what) in [
        (PR, true, "Approve on the board"),
        (PUBLISH_PR, true, "Publish on the board"),
        (TAILNET_PR, true, "an allowlisted tailnet login"),
        (90, false, "a terminal approval"),
        (91, true, "a terminal record in the board's shape"),
    ] {
        let seen = read_state(&board, &operator, pr);
        assert_eq!(seen["state"], "in-force", "{what}: {seen}");
        assert_eq!(
            seen["board_revocable"],
            json!(want),
            "{what}: board_revocable must be a boolean that says whether the board may revoke: {seen}"
        );
        let id = seen["approval_id"].as_str().unwrap().to_string();
        let events = board.approval_events();
        let (accepted, status, body) = board_revoke(&board, &operator, &id);
        assert_eq!(
            accepted, want,
            "{what}: board_revocable={want} but the board revoke answered {status} {body}"
        );
        let after = read_state(&board, &operator, pr);
        if want {
            assert_eq!(after["state"], "revoked", "{what}: {after}");
            assert!(
                after.get("board_revocable").is_none(),
                "a revoked approval carries no flag: {after}"
            );
        } else {
            assert!((400..500).contains(&status), "{what}: {status} {body}");
            assert_eq!(
                board.approval_events(),
                events,
                "{what}: a refused revoke wrote"
            );
            assert_eq!(after, seen, "{what}: a refused revoke changed the read");
        }
    }
    // Another action's record never reads as in-force for the head, so it
    // offers nothing; the board revoke refuses it and writes nothing.
    let deploy = rpc(
        "approval_record",
        json!({"repo": REPO, "pr": 92, "head": HEAD, "action": "deploy",
               "source": "operator (ui) via board"}),
    )
    .unwrap()["approval_id"]
        .as_str()
        .unwrap()
        .to_string();
    let seen = read_state(&board, &operator, 92);
    assert_eq!(seen["state"], "missing", "{seen}");
    assert!(seen.get("board_revocable").is_none(), "{seen}");
    let events = board.approval_events();
    let (accepted, status, body) = board_revoke(&board, &operator, &deploy);
    assert!(
        !accepted,
        "the board revoked a deploy record: {status} {body}"
    );
    assert_eq!(board.approval_events(), events);
}

// ---------------------------------------------------------------------
// Round 5 (operator decision on CAD-1218, 2026-10-09, superseding round
// 2's "public owner sessions are refused"): every platform-verified
// `owner` session is an approver, with no configuration; a `member`
// never is. The approver rule admits exactly three kinds of actor:
// `operator (ui)` (loopback), `<login> (tailscale)` for a login in
// `pm.yaml` `approvals.tailnet_logins`, and a platform owner as the board
// names it, `<name> <email> (board)`.
//
// Contract:
// - Board: the approve, revoke and state routes accept `Caller::Operator`
//   and `Caller::Named(n)` with `n.operator` (the verified `owner` role,
//   `board_identity::Role::is_operator`), relaying `request_actor =
//   n.actor` — the string `board_caller` derives from the verified
//   assertion (`<name> <email> (board)`), never from the request.
//   Publish already relays `caller.actor()`. A `member` session never
//   reaches a handler: `admit` / `admit_operator_read` refuse it
//   (`member_role`), and nothing is recorded or merged.
// - Daemon: `approver_source` additionally admits an actor of the form
//   `<name> <<email>> (board)`; the source is `"<actor> via board"`, so
//   the record is `board_revocable`.
// - Every daemon verb still requires `operator_connection`: an agent or
//   unproven caller that dials it with a `(board)` actor is refused.
// - The actor comes only from the connection: a body `request_actor` is
//   refused at the board, and a public session is never attributed to a
//   `Tailscale-User-Login` header.
// ---------------------------------------------------------------------

const OWNER_ACTOR: &str = "Platform Owner <owner@example.com> (board)";

#[test]
#[ignore = "CAD-1218: enabled by the implementation"]
fn cad1218_platform_owner_is_an_approver_and_member_never() {
    let root = tempfile::Builder::new().prefix("c1218o").tempdir().unwrap();
    let board = Board::start(root.path());
    let operator = board.session();
    let publish = |issue: &str| format!("/api/delivery/{issue}/merge");
    let publish_body = json!({"sha": HEAD}).to_string();
    let approve_pr = |pr: u64| json!({"repo": REPO, "pr": pr, "head": HEAD}).to_string();

    // --- the verbs refuse a direct dial even with a (board) actor ---
    for (method, params) in [
        (
            VERB,
            json!({"repo": REPO, "pr": PUBLIC_PR, "head": HEAD, "request_actor": OWNER_ACTOR}),
        ),
        (
            "delivery_approve",
            json!({"issue": PUBLISH_ISSUE, "sha": HEAD, "request_actor": OWNER_ACTOR}),
        ),
        (
            STATE_VERB,
            json!({"repo": REPO, "pr": PUBLIC_PR, "head": HEAD, "request_actor": OWNER_ACTOR}),
        ),
        (
            REVOKE_VERB,
            json!({"id": "merge-pr44-x", "reason": "r", "request_actor": OWNER_ACTOR}),
        ),
    ] {
        for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
            let text = scoped(who.clone(), || {
                crate::client::rpc(&board.state, method, params.clone())
            })
            .expect_err("a non-operator recorded by naming a (board) actor")
            .to_string();
            assert!(
                !text.contains("Unknown method"),
                "{method} does not exist: {text}"
            );
            assert!(
                text.contains("operator action"),
                "{who:?} on {method}: {text}"
            );
        }
    }
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "missing");
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "");

    // --- the (board) kind is the board's own shape, `<name> <email> (board)`;
    // even an operator connection cannot pass a bare or tailnet-looking one ---
    for actor in ["x (board)", "mallory@example.com (board)", " <> (board)"] {
        let text = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &board.state,
                VERB,
                json!({"repo": REPO, "pr": PUBLIC_PR, "head": HEAD, "request_actor": actor}),
            )
        })
        .expect_err("a malformed (board) actor recorded an approval")
        .to_string();
        assert!(text.contains("approval allowlist"), "'{actor}': {text}");
    }
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "missing");

    // --- forged actors at the board: the body never names the actor ---
    let mut forged: Value = serde_json::from_str(&approve_pr(PUBLIC_PR)).unwrap();
    forged["request_actor"] = json!(OWNER_ACTOR);
    let (status, body) = board.post("operator", Some(&operator), APPROVE, &forged.to_string());
    assert!((400..500).contains(&status), "{status} {body}");
    let forged_publish = json!({"sha": HEAD, "request_actor": OWNER_ACTOR}).to_string();
    let (status, body) = board.post(
        "operator",
        Some(&operator),
        &publish(PUBLISH_ISSUE),
        &forged_publish,
    );
    assert!((400..500).contains(&status), "{status} {body}");
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "");

    // --- a platform member: refused by admission at all four routes ---
    let member = board.public_session("member");
    let (status, meta) = board.public("/api/meta", &member, None);
    assert_eq!((status, &meta["signed_in"]), (200, &json!(true)), "{meta}");
    for (path, body) in [
        (APPROVE.to_string(), Some(approve_pr(PUBLIC_PR))),
        (publish(PUBLISH_ISSUE), Some(publish_body.clone())),
        (state_path(REPO, PUBLIC_PR, HEAD), None),
        (
            "/api/approvals/merge-pr44-x/revoke".to_string(),
            Some(json!({"reason": "member"}).to_string()),
        ),
    ] {
        let (status, reply) = board.public(&path, &member, body.as_deref());
        assert_eq!(status, 403, "a member on {path}: {reply}");
        assert_eq!(reply["check"], "member_role", "a member on {path}: {reply}");
    }
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "missing");
    assert_eq!(board.audit_pr(PUBLISH_PR, HEAD)["state"], "missing");
    assert_eq!(merges(&board), "", "a member's Publish enqueued a merge");

    // --- a platform owner: an approver on all four routes, no config ---
    let owner = board.public_session("owner");
    let (status, meta) = board.public("/api/meta", &owner, None);
    assert_eq!((status, &meta["signed_in"]), (200, &json!(true)), "{meta}");
    let read = state_path(REPO, PUBLIC_PR, HEAD);
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(status, 200, "the owner reads: {body}");
    assert_eq!(body["state"], "missing", "{body}");
    let (status, body) = board.public(APPROVE, &owner, Some(&approve_pr(PUBLIC_PR)));
    assert_eq!(status, 200, "the owner approves the shown head: {body}");
    let id = body["approval_id"].as_str().unwrap().to_string();
    let seen = board.audit_pr(PUBLIC_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["approval_id"], id.as_str(), "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    let source = seen["source"].as_str().unwrap_or_default();
    assert!(
        source.contains(OWNER_ACTOR),
        "the source names the verified owner: {seen}"
    );
    assert!(
        !source.contains("tailscale") && !source.contains("mallory"),
        "a public session is never a tailnet login: {seen}"
    );
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["state"], "in-force", "{body}");
    assert_eq!(body["approval_id"], id.as_str(), "{body}");
    assert_eq!(body["board_revocable"], true, "{body}");
    let (status, body) = board.public(
        &format!("/api/approvals/{id}/revoke"),
        &owner,
        Some(&json!({"reason": "owner re-review"}).to_string()),
    );
    assert_eq!(status, 200, "the owner revokes their approval: {body}");
    assert_eq!(board.audit_pr(PUBLIC_PR, HEAD)["state"], "revoked");
    let (status, body) = board.public(&read, &owner, None);
    assert_eq!((status, &body["state"]), (200, &json!("revoked")), "{body}");
    // Publish: recorded and enqueued, attributed to the owner.
    let (status, body) = board.public(&publish(PUBLISH_ISSUE), &owner, Some(&publish_body));
    assert_eq!(status, 200, "the owner Publishes: {body}");
    let seen = board.audit_pr(PUBLISH_PR, HEAD);
    assert_eq!(seen["state"], "in-force", "{seen}");
    assert_eq!(seen["recorded_via"], "operator-connection", "{seen}");
    let source = seen["source"].as_str().unwrap_or_default();
    assert!(source.contains(OWNER_ACTOR), "{seen}");
    assert!(!source.contains("tailscale"), "{seen}");
    assert!(merges(&board).contains(&format!("merge {PUBLISH_PR} ")));
    assert!(
        !merges(&board).contains(&format!("merge {PUBLISH_PR_2} ")),
        "only the owner's Publish enqueued"
    );
}
