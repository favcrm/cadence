//! CAD-1041 result checks: the operator sends one approved post now,
//! exactly once, and nobody else can. A real in-process daemon and
//! board; the provider door is a counting `PublishSender` over the
//! library's fake ledger. Each test names the ticket outcome it proves.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::publish::{self as door, FakeProviderBehavior};
use cadence_agent::platform::agenticos_external::publish::{LedgerOutcome, Preflight, Refusal};
use cadence_agent::test_seam::{scoped, Asserted};
use cadence_agent::{client, daemon, store::social_publish::NewSocialPublish, store::Store};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const CONN: &str = "connA_harbour_fb";
const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_synthetic_grant_fb";
const NOW: i64 = 1_800_000_000;
type Daemon = (
    Arc<AtomicBool>,
    std::thread::JoinHandle<cadence_agent::Result<()>>,
);

/// The provider door: staging, the exec POST (counted) and status.
struct Door {
    ledger: door::FakePublishLedger,
    grant: Mutex<door::SendGrant>,
    execs: AtomicU64,
    /// Scripted staging: "" asks the ledger, else "refused"/"uncertain".
    staging: Mutex<&'static str>,
    lose_response: AtomicBool,
    crash_on_status: AtomicBool,
    /// Two-party rendezvous inside staging: both presses arrive first.
    meet: Option<(Mutex<u32>, Condvar)>,
}

fn destination() -> door::Destination {
    door::Destination {
        connection_id: CONN.into(),
        toolkit: door::Toolkit::Facebook,
        display_name: "Harbour".into(),
        destination_id: DEST.into(),
        status_active: true,
        available: true,
    }
}

impl door::PublishSender for Door {
    fn preflight(&self, b: &door::SendBinding) -> Preflight {
        if let Some((count, cv)) = &self.meet {
            *count.lock().unwrap() += 1;
            cv.notify_all();
            let met =
                cv.wait_timeout_while(count.lock().unwrap(), Duration::from_secs(20), |n| *n < 2);
            assert_eq!(*met.unwrap().0, 2, "the presses never overlapped");
        }
        let grant = self.grant.lock().unwrap();
        match *self.staging.lock().unwrap() {
            "refused" => Preflight::Refused(Refusal::new("not_publishable", "door refused")),
            "uncertain" => Preflight::Uncertain(Refusal::new("staging_timeout", "no answer")),
            _ => match self.ledger.preflight(b, &destination(), &grant, "ws", NOW) {
                Ok(_) => Preflight::Approved,
                Err(refusal) => Preflight::Refused(refusal),
            },
        }
    }
    fn execute(&self, b: &door::SendBinding) -> Result<LedgerOutcome, Refusal> {
        self.execs.fetch_add(1, SeqCst);
        let how = match self.lose_response.load(SeqCst) {
            true => FakeProviderBehavior::LoseResponseAfterAccept,
            false => FakeProviderBehavior::Post,
        };
        let mut grant = self.grant.lock().unwrap();
        self.ledger
            .execute(b, &destination(), &mut grant, "ws", NOW, how)
    }
    fn status(&self, key: &str) -> Result<LedgerOutcome, Refusal> {
        assert!(
            !self.crash_on_status.load(SeqCst),
            "daemon died after the door accepted"
        );
        self.ledger.status(key)
    }
}

/// A daemon state dir with one door that outlives any daemon on it.
struct Fx {
    root: tempfile::TempDir,
    door: Arc<Door>,
    clock: Arc<AtomicI64>,
    daemon: Option<Daemon>,
}

impl Fx {
    fn new(meet: bool) -> Self {
        let grant = door::SendGrant {
            id: GRANT.into(),
            workspace_id: "ws".into(),
            connection_id: CONN.into(),
            destination_id: DEST.into(),
            toolkit: door::Toolkit::Facebook,
            caption_digest: caption(),
            image_digest: None,
            cadence_approval_id: "appr-1".into(),
            max_uses: 5,
            remaining_uses: 5,
            revoked: false,
            not_before_epoch: 0,
            expires_at_epoch: i64::MAX,
        };
        let door = Arc::new(Door {
            ledger: door::FakePublishLedger::enabled(),
            grant: Mutex::new(grant),
            execs: AtomicU64::new(0),
            staging: Mutex::new(""),
            lose_response: AtomicBool::new(false),
            crash_on_status: AtomicBool::new(false),
            meet: meet.then(Default::default),
        });
        let root = tempfile::Builder::new().prefix("c1041").tempdir().unwrap();
        let clock = Arc::new(AtomicI64::new(NOW));
        Self {
            root,
            door,
            clock,
            daemon: None,
        }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().to_path_buf()
    }
    /// A queued, approved intent due at `due` (seeded while no daemon runs).
    fn queue(&self, request: &str, due: i64) -> String {
        let cap = caption();
        let row = NewSocialPublish {
            request_id: request,
            install_id: "install-a",
            context_id: Some("ctx-a"),
            run_id: "run-1",
            effect_id: "fx-1",
            artifact_id: None,
            bundle_digest: None,
            slot: None,
            connection_id: "con_harbour_fb",
            aos_connection_id: Some(CONN),
            destination_id: DEST,
            toolkit: "facebook",
            caption_digest: &cap,
            image_digest: None,
            media_key: None,
            grant_id: GRANT,
            approval_id: request,
            due_epoch: due,
            claim_after_epoch: self.clock.load(SeqCst) - 5,
            timezone: "Asia/Hong_Kong",
        };
        let store = Store::open(&self.dir().join("cadence.sqlite3")).unwrap();
        let intent = store.social_publish_schedule(&row).unwrap()["intent"].clone();
        intent["intent_id"].as_str().unwrap().into()
    }
    fn start(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", dir.join("no-pm").to_str().unwrap());
        let clock = Arc::clone(&self.clock);
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            operator_clock: Some(Arc::new(move || clock.load(SeqCst))),
            social_publish_sender: Some(self.door.clone()),
            ..Default::default()
        };
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }
    fn stop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.dir(), method, params))
    }
    fn send_now(&self, who: Asserted, id: &str) -> cadence_agent::Result<Value> {
        self.rpc(who, "social_publish_send_now", send_params(id))
    }
    fn intent(&self, id: &str) -> Value {
        let shown = self.rpc(
            Asserted::Operator,
            "social_publish_show",
            json!({"intent_id": id}),
        );
        shown.unwrap()["intent"].clone()
    }
    /// What the outside world sees: exec POSTs at the door, and `id`'s state.
    fn seen(&self, id: &str) -> (u64, Value) {
        (
            self.door.execs.load(SeqCst),
            self.intent(id)["state"].clone(),
        )
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop();
    }
}

fn caption() -> String {
    door::caption_digest_of("Harbour at dawn")
}

fn send_params(id: &str) -> Value {
    json!({"intent_id": id, "install_id": "install-a", "context_id": "ctx-a"})
}

/// R1: the operator's send-now on a queued, approved intent posts it once
/// and the intent reads posted, its receipt verified against what the
/// door returned. Another due intent is untouched; a second press sends
/// nothing.
#[test]
fn r1_operator_send_now_posts_the_named_intent_once() {
    let mut fx = Fx::new(false);
    let (id, other) = (fx.queue("r1-post", NOW + 3600), fx.queue("r1-other", NOW));
    fx.start();
    let reply = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("posted")), "{reply:?}");
    let sent = fx.intent(&id);
    assert_eq!(
        sent["receipt"], sent["upstream"],
        "receipt is the door's evidence"
    );
    assert_eq!(fx.seen(&other), (1, json!("queued")));
    let again = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("posted")), "{again:?}");
}

/// R2: two presses at once publish once. Both pass the queued check and
/// meet inside staging before either claims.
#[test]
fn r2_two_presses_at_once_publish_once() {
    let mut fx = Fx::new(true);
    let id = fx.queue("r2-double", NOW);
    fx.start();
    let press = |dir: std::path::PathBuf, id: String| {
        let send = move || client::rpc(&dir, "social_publish_send_now", send_params(&id));
        std::thread::spawn(move || scoped(Asserted::Operator, send))
    };
    let presses = [press(fx.dir(), id.clone()), press(fx.dir(), id.clone())];
    let replies: Vec<_> = presses.into_iter().map(|p| p.join().unwrap()).collect();
    assert_eq!(fx.seen(&id), (1, json!("posted")), "{replies:?}");
}

/// R3: an agent, a detached child (no provable identity) and a board
/// member are refused over the RPC and the board's HTTP route, and
/// nothing is published; the operator then sends the same row.
#[test]
fn r3_agent_and_member_are_refused_over_rpc_and_http() {
    use base64::Engine;
    use ring::signature::KeyPair;
    let mut fx = Fx::new(false);
    let id = fx.queue("r3-gate", NOW);
    fx.start();
    for who in [Asserted::Agent("cc13-pw".into()), Asserted::Unproven] {
        let reply = fx.send_now(who.clone(), &id);
        assert_eq!(fx.seen(&id), (0, json!("queued")), "{who:?} {reply:?}");
    }
    // A public board whose platform issuer is a local JWKS stub.
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let signer = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[0x41; 32]).unwrap();
    let x = b64.encode(signer.public_key().as_ref());
    let jwks = json!({"keys": [{"kty": "OKP", "crv": "Ed25519", "kid": "k1", "x": x}]}).to_string();
    let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let issuer = format!("http://{}", stub.server_addr().to_ip().unwrap());
    std::thread::spawn(move || {
        for request in stub.incoming_requests() {
            let _ = request.respond(tiny_http::Response::from_string(jwks.clone()));
        }
    });
    let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
    let port = (3110..3200).find(free).unwrap();
    let (public, local) = (
        format!("acme.board.localhost:{port}"),
        format!("cadence-{port}.localhost:{port}"),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let public_board = cadence_agent::ui::PublicBoard {
        host: public.clone(),
        authorize_url: format!("{issuer}/v2/board/authorize"),
        issuer: issuer.clone(),
        company: "co_1".into(),
        company_slug: None,
    };
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(stop.clone()),
        startup: Some(tx),
        test_seam: true,
        allow_hosts: vec![public.clone()],
        allow_origins: vec![format!("http://{public}")],
        public: Some(public_board),
        ..Default::default()
    };
    let (dir, pm) = (fx.dir(), fx.dir().join("no-pm"));
    let board = std::thread::spawn(move || cadence_agent::ui::serve(&dir, &pm, &opts));
    rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    let post = |host: &str, path: &str, extra: &str, body: &str| -> (u16, String) {
        let req = format!(
            "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\nX-Cadence-Board: 1\r\n\
             Sec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n{extra}Content-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut s, &mut text).unwrap();
        (
            text.split_whitespace().nth(1).unwrap().parse().unwrap(),
            text,
        )
    };
    let route = format!("/api/social-publishes/{id}/send-now");
    let scope = json!({"install_id": "install-a", "context_id": "ctx-a"}).to_string();
    // An agent's HTTP request is refused by the route's class.
    let token = cadence_agent::test_seam::Seam::token_at(&fx.dir()).unwrap();
    let agent = format!("X-Cadence-Test-As: agent:cc13-pw\r\nX-Cadence-Test-Token: {token}\r\n");
    assert_eq!(
        post(&local, &route, &agent, &scope).0,
        403,
        "agent over HTTP"
    );
    // A platform-verified `member` session is refused by its role. The
    // board judges assertion lifetimes on the daemon's operator clock.
    let claims = json!({"iss": issuer, "aud": public, "sub": "usr_member", "email": "m@example.com",
        "name": "Member", "company": "co_1", "role": "member", "iat": NOW - 5, "exp": NOW + 30,
        "jti": "jti-r3-member"});
    let head = b64.encode(json!({"alg": "EdDSA", "typ": "JWT", "kid": "k1"}).to_string());
    let signed = format!("{head}.{}", b64.encode(claims.to_string()));
    let assertion = format!("{signed}.{}", b64.encode(signer.sign(signed.as_bytes())));
    let body = json!({"assertion": assertion}).to_string();
    let (code, opened) = post(&public, "/__platform/session", "", &body);
    assert_eq!(code, 200, "member session: {opened}");
    let cookie = opened
        .lines()
        .find_map(|l| l.strip_prefix("Set-Cookie: "))
        .unwrap();
    let cookie = format!("Cookie: {}\r\n", cookie.split(';').next().unwrap());
    assert_eq!(
        post(&public, &route, &cookie, &scope).0,
        403,
        "member over HTTP"
    );
    stop.store(true, SeqCst);
    let _ = board.join();
    assert_eq!(fx.seen(&id), (0, json!("queued")));
    let reply = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("posted")), "{reply:?}");
}

/// R4: a revoked grant, or a refused or uncertain staging answer,
/// publishes nothing; the intent's state says which.
#[test]
fn r4_revoked_refused_or_uncertain_publishes_nothing() {
    let mut fx = Fx::new(false);
    let ids = ["r4-revoked", "r4-refused", "r4-uncertain"].map(|r| fx.queue(r, NOW));
    fx.start();
    // (grant revoked, scripted staging, the state that says why)
    let cases = [
        (true, "", "refused"),
        (false, "refused", "refused"),
        (false, "uncertain", "queued"),
    ];
    for (id, (revoked, staging, why)) in ids.iter().zip(cases) {
        fx.door.grant.lock().unwrap().revoked = revoked;
        *fx.door.staging.lock().unwrap() = staging;
        let reply = fx.send_now(Asserted::Operator, id);
        assert_eq!(
            fx.seen(id),
            (0, json!(why)),
            "{revoked} {staging}: {reply:?}"
        );
    }
    let refusal = fx.intent(&ids[0])["receipt"]["error"].clone();
    assert!(
        refusal.as_str().unwrap().contains("grant_revoked"),
        "{refusal}"
    );
}

/// R5: the daemon dies after the door accepted the post. On restart a
/// new press sends nothing, and reconcile settles it from status alone.
#[test]
fn r5_crash_after_door_accept_never_resends_on_restart() {
    let mut fx = Fx::new(false);
    let id = fx.queue("r5-crash", NOW);
    fx.door.lose_response.store(true, SeqCst);
    fx.door.crash_on_status.store(true, SeqCst);
    fx.start();
    let crashed = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("processing")), "{crashed:?}");
    fx.stop();
    fx.door.crash_on_status.store(false, SeqCst);
    fx.start();
    let again = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("processing")), "{again:?}");
    let settled = fx.rpc(
        Asserted::Operator,
        "social_publish_reconcile",
        json!({"intent_id": id}),
    );
    assert_eq!(
        fx.door.execs.load(SeqCst),
        1,
        "reconcile re-sent: {settled:?}"
    );
    assert_eq!(settled.unwrap()["intent"]["upstream"]["state"], "posted");
}

/// R6: an intent more than 15 minutes overdue is not sent; at exactly 15
/// minutes it still is. Due is in the wall-clock future, so only the
/// daemon's operator clock can make it late.
#[test]
fn r6_more_than_fifteen_minutes_overdue_is_not_sent() {
    let mut fx = Fx::new(false);
    let due = NOW + 365 * 86_400;
    let id = fx.queue("r6-late", due);
    fx.start();
    fx.clock.store(due + 15 * 60 + 1, SeqCst);
    let late = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (0, json!("queued")), "{late:?}");
    fx.clock.store(due + 15 * 60, SeqCst);
    let on_time = fx.send_now(Asserted::Operator, &id);
    assert_eq!(fx.seen(&id), (1, json!("posted")), "{on_time:?}");
}
