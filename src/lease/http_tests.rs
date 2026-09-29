//! CAD-673 adversarial transport tests. The loopback endpoint is constructed
//! directly here; production configuration must only admit lease.internal.
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

struct Peer {
    url: String,
    requests: mpsc::Receiver<String>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Peer {
    fn new(responses: Vec<String>, delay: Duration) -> Self {
        Self::staggered(responses.into_iter().map(|body| (body, delay)).collect())
    }

    /// One `(response, hold-before-responding)` pair per accepted POST —
    /// lets a test admit fast, stall exactly one renewal past its
    /// two-second attempt budget, then heal, without slowing the rest.
    fn staggered(schedule: Vec<(String, Duration)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/renew", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halted = Arc::clone(&stop);
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut replies = schedule.into_iter();
            while !halted.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        socket
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = Vec::new();
                        let mut byte = [0];
                        while socket.read(&mut byte).unwrap_or(0) == 1 {
                            request.push(byte[0]);
                            if request.ends_with(b"\r\n\r\n") {
                                break;
                            }
                            assert!(request.len() < 8192);
                        }
                        let _ = tx.send(String::from_utf8(request).unwrap());
                        // A real host answers concurrently: a stalled
                        // reply must not head-of-line-block the next
                        // renewal behind it, or a heal would always
                        // arrive "late". Replies stay assigned in
                        // accept order; only the wait-and-respond runs
                        // per connection. Detached: a sleeper whose
                        // client already timed out writes to a closed
                        // socket and exits.
                        if let Some((response, delay)) = replies.next() {
                            let halted = Arc::clone(&halted);
                            thread::spawn(move || {
                                let deadline = Instant::now() + delay;
                                while Instant::now() < deadline && !halted.load(Ordering::SeqCst) {
                                    thread::sleep(Duration::from_millis(5));
                                }
                                let _ = socket.write_all(response.as_bytes());
                            });
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("peer accept: {e}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(worker),
        }
    }

    fn provider(&self) -> HttpProvider {
        HttpProvider::new(self.url.clone(), HOST_TTL)
    }

    fn request(&self) -> String {
        self.requests.recv_timeout(Duration::from_secs(3)).unwrap()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn reply(status: u16, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Test\r\nConnection: close\r\nContent-Length: {}\r\n{headers}\r\n{body}",
        body.len()
    )
}

#[test]
fn cad673_first204_admits_and_wire_has_no_identity_fields() {
    let peer = Peer::new(
        vec![reply(204, "X-Lease-Epoch: 987\r\n", ""), reply(204, "", "")],
        Duration::ZERO,
    );
    let provider = peer.provider();
    let held = provider
        .acquire("forged-company:foreign-instance:token", 998)
        .unwrap();
    assert_eq!(held.epoch, None, "forged response generation accepted");
    provider.renew(&held).unwrap();
    for _ in 0..2 {
        let request = peer.request().to_ascii_lowercase();
        assert!(request.starts_with("post /renew http/1.1\r\n"), "{request}");
        for forbidden in [
            "authorization:",
            "cookie:",
            "company",
            "instance",
            "epoch",
            "998",
            "token",
        ] {
            assert!(!request.contains(forbidden), "identity leaked: {request}");
        }
        assert!(request.ends_with("\r\n\r\n"));
        assert!(!request.contains("transfer-encoding:"));
        assert!(!request.contains("content-length:") || request.contains("content-length: 0\r\n"));
    }
    provider.release(&held).unwrap();
    assert!(
        peer.requests
            .recv_timeout(Duration::from_millis(50))
            .is_err(),
        "release contacted host"
    );
}

#[test]
fn cad673_only_host_bound_endpoint_is_admitted() {
    for endpoint in [
        "http://lease.internal",
        "http://lease.internal/",
        "http://lease.internal/renew",
    ] {
        assert!(Spec::parse(endpoint).is_ok(), "{endpoint}");
    }
    for endpoint in [
        "http://127.0.0.1/renew",
        "https://lease.internal/renew",
        "http://lease.internal.evil/renew",
        "http://user:secret@lease.internal/renew",
        "http://lease.internal:80/renew",
        "http://lease.internal/other",
        "http://lease.internal/renew?company=other",
        "http://lease.internal/renew#token",
        "http://lease.internal\\@evil/renew",
    ] {
        assert!(
            Spec::parse(endpoint).is_err(),
            "foreign/malformed endpoint accepted: {endpoint}"
        );
    }
}

#[test]
fn cad673_wrong_status_and_forged_success_body_refuse() {
    for status in [200, 201, 400, 403, 409, 500, 503] {
        let peer = Peer::new(
            vec![reply(
                status,
                "X-Lease-Epoch: 987\r\n",
                "{\"ok\":true,\"epoch\":987}",
            )],
            Duration::ZERO,
        );
        assert!(
            peer.provider().acquire("h", 0).is_err(),
            "HTTP {status} admitted"
        );
    }
}

#[test]
fn cad673_redirect_never_reaches_foreign_peer() {
    let foreign = Peer::new(vec![reply(204, "", "")], Duration::ZERO);
    let origin = Peer::new(
        vec![reply(307, &format!("Location: {}\r\n", foreign.url), "")],
        Duration::ZERO,
    );
    assert!(origin.provider().acquire("h", 0).is_err());
    assert!(foreign
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
}

#[test]
fn cad673_timeout_is_bounded_and_response_was_attempted() {
    let peer = Peer::new(vec![reply(204, "", "")], Duration::from_secs(4));
    let started = Instant::now();
    assert!(peer.provider().acquire("h", 0).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "unbounded request"
    );
    peer.request();
}

#[test]
fn cad673_stale409_cannot_be_reacquired_by_same_provider() {
    let peer = Peer::new(
        vec![
            reply(204, "", ""),
            reply(409, "", "{\"error\":\"lease_lost\"}"),
            reply(204, "", ""),
        ],
        Duration::ZERO,
    );
    let provider = peer.provider();
    let lease = provider.acquire("h", 0).unwrap();
    assert!(provider.renew(&lease).is_err());
    assert!(provider.acquire("forged-new-holder", 999).is_err());
    assert!(provider.renew(&lease).is_err());
    peer.request();
    peer.request();
    assert!(
        peer.requests
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "fenced provider contacted host again"
    );
}

#[test]
fn cad673_forged_expiry_cannot_restore_expired_host_lease() {
    let peer = Peer::new(vec![reply(204, "", ""), reply(204, "", "")], Duration::ZERO);
    let provider = peer.provider();
    let mut lease = provider.acquire("h", 0).unwrap();
    peer.request();
    // Deterministically expire the authoritative clock without sleeping.
    provider.state.lock().unwrap().deadline = Some(Instant::now() - Duration::from_secs(1));
    lease.expires_monotonic = Some(Instant::now() + Duration::from_secs(600));
    lease.expires_unix = now_unix() + 600.0;
    lease.epoch = Some(999);
    lease.holder = "foreign-company:other-instance".into();
    assert!(provider.renew(&lease).is_err());
    assert!(peer
        .requests
        .recv_timeout(Duration::from_millis(100))
        .is_err());
}

#[test]
fn cad673_late204_cannot_revive_expired_admission() {
    let peer = Peer::new(vec![reply(204, "", "")], Duration::from_millis(200));
    let provider = HttpProvider::new(peer.url.clone(), Duration::from_millis(75));
    assert!(provider.acquire("h", 0).is_err());
    peer.request();
    assert!(provider.acquire("forged", 999).is_err());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn cad673_host_generation_absent_and_shared_writes_fenced() {
    let peer = Peer::new(
        vec![reply(204, "X-Lease-Epoch: 999\r\n", ""), reply(409, "", "")],
        Duration::ZERO,
    );
    let dir = tempfile::TempDir::new().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    // Existing file-generation history must not be replaced by a fabricated
    // host generation, or lost if this state later returns to a file provider.
    std::fs::write(state.join("lease-epoch"), "41\n").unwrap();
    let ctl = start_lease(
        &state,
        Arc::new(peer.provider()),
        HOST_RENEW_URL.into(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(ctl.epoch(), None);
    assert_eq!(ctl.pm_lease().epoch(), None);
    assert!(ctl.status_json()["epoch"].is_null());
    assert_eq!(ctl.status_json()["epoch_available"], false);
    assert_eq!(
        std::fs::read_to_string(state.join("lease-epoch")).unwrap(),
        "41\n"
    );
    let store = Arc::new(crate::store::Store::open(&state.join("cadence.sqlite3")).unwrap());
    store.install_write_fence(ctl.fence());
    store
        .register_agent(&crate::store::NewAgent {
            alias: "worker",
            provider: "fake",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let registration = store.events("worker", 0, 100).unwrap();
    assert_eq!(registration.len(), 1);
    assert_eq!(registration[0].kind, "registered");
    let registration_seq = registration[0].seq;
    store.event_public("worker", "before", json!({})).unwrap();
    let before_events = store.events("worker", registration_seq, 100).unwrap();
    assert_eq!(before_events.len(), 1);
    assert_eq!(before_events[0].kind, "before");
    let pm_dir = dir.path().join("pm");
    let mut pm = crate::issue::Pm::init(&pm_dir).unwrap();
    pm.attach_lease(ctl.pm_lease());
    let note = pm_dir.join("note.md");
    std::fs::write(&note, "admitted\n").unwrap();
    pm.commit(std::slice::from_ref(&note), "host write\n")
        .unwrap();
    assert!(!git(&pm_dir, &["log", "-1", "--format=%B"]).contains("Lease-Epoch:"));
    std::fs::write(pm_dir.join("pending.md"), "pending\n").unwrap();
    git(&pm_dir, &["add", "pending.md"]);
    pm.flush_pending("test").unwrap();
    assert!(!git(&pm_dir, &["log", "-1", "--format=%B"]).contains("Lease-Epoch:"));
    let before = git(&pm_dir, &["rev-parse", "HEAD"]);
    assert!(ctl.renew().is_err());
    assert!(ctl.fence().tripped());
    assert!(ctl.renew().is_err(), "a second renewal revived the fence");
    let writers: Vec<_> = (0..4)
        .map(|i| {
            let store = Arc::clone(&store);
            thread::spawn(move || {
                store.event_public("worker", "after", json!({"forged_epoch": 999, "caller": i}))
            })
        })
        .collect();
    for writer in writers {
        assert!(writer.join().unwrap().is_err());
    }
    let after_events = store.events("worker", registration_seq, 100).unwrap();
    assert_eq!(after_events.len(), 1);
    assert_eq!(after_events[0].kind, "before");
    assert_eq!(after_events[0].seq, before_events[0].seq);
    assert_eq!(store.events("worker", 0, 100).unwrap().len(), 2);
    std::fs::write(&note, "refused\n").unwrap();
    assert!(pm
        .commit(std::slice::from_ref(&note), "forged\nLease-Epoch: 999\n")
        .is_err());
    assert!(pm.lock().is_err());
    assert!(pm.try_lock().is_err());
    assert!(pm.flush_pending("test").is_err());
    assert_eq!(git(&pm_dir, &["rev-parse", "HEAD"]), before);
    assert_eq!(git(&pm_dir, &["diff", "--cached", "--name-only"]), "");
}

#[test]
fn cad673_monotonic_deadline_controls_host_write_fence() {
    let fence = Fence::default();
    fence.set_expiry(now_unix() + 600.0);
    *fence.expires_monotonic.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
    assert!(fence.check().unwrap().contains("deadline expired"));
}

#[test]
fn cad673_host_ttl_cannot_exceed_bridge_policy() {
    let dir = tempfile::TempDir::new().unwrap();
    let hosted = Hosted {
        lease: Some("http://lease.internal".into()),
        lease_ttl_secs: Some(30),
        ..Hosted::default()
    };
    assert!(acquire(dir.path(), &hosted)
        .err()
        .unwrap()
        .to_string()
        .contains("6s"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// CAD-702: a 500 inside the six-second local deadline is a blip, not
/// loss. The renewal surfaces but does not fence; health says
/// retrying; a later 204 heals and clears the retry state.
#[test]
fn cad702_blip_then_204_heals_without_fencing() {
    let peer = Peer::new(
        vec![
            reply(204, "", ""),
            reply(500, "", "overloaded"),
            reply(204, "", ""),
        ],
        Duration::ZERO,
    );
    let dir = tempfile::TempDir::new().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let ctl = start_lease(
        &state,
        Arc::new(peer.provider()),
        HOST_RENEW_URL.into(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )
    .unwrap();
    // The blip surfaces — the 500 is reported, not swallowed...
    let err = ctl.renew().unwrap_err();
    assert!(err.to_string().contains("500"), "{err}");
    // ...but fences nothing: the deadline from the last 204 is open.
    assert!(!ctl.fence().tripped(), "a single 500 fenced the daemon");
    assert!(ctl.fence().check().is_none());
    // Health tells the truth: unfenced yet retrying, with the cause.
    let health = ctl.status_json();
    assert!(health["fenced"].is_null(), "{health}");
    assert_eq!(health["retrying"], true, "{health}");
    assert!(
        health["last_renew_error"]
            .as_str()
            .unwrap_or_default()
            .contains("500"),
        "{health}"
    );
    // A 204 inside the deadline heals: renewal succeeds, the retry
    // state clears, and the fence never tripped in between.
    ctl.renew().unwrap();
    assert!(!ctl.fence().tripped());
    let health = ctl.status_json();
    assert_eq!(health["retrying"], false, "{health}");
    assert!(health["last_renew_error"].is_null(), "{health}");
    // Three POSTs: admission, blip, heal — the blip never fenced the
    // provider, so the heal was attempted, not refused.
    peer.request();
    peer.request();
    peer.request();
}

/// CAD-702: 409 is definite loss even with the whole six-second
/// deadline still open — it latches at once, trips the fence, and the
/// provider never contacts the host again.
#[test]
fn cad702_409_fences_immediately_with_open_deadline() {
    let peer = Peer::new(
        vec![
            reply(204, "", ""),
            reply(409, "", "{\"error\":\"lease_lost\"}"),
            reply(204, "", ""),
        ],
        Duration::ZERO,
    );
    let dir = tempfile::TempDir::new().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let ctl = start_lease(
        &state,
        Arc::new(peer.provider()),
        HOST_RENEW_URL.into(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )
    .unwrap();
    let err = ctl.renew().unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
    assert!(ctl.fence().tripped(), "a 409 did not fence the daemon");
    let health = ctl.status_json();
    assert!(health["fenced"].is_string(), "{health}");
    assert_eq!(health["retrying"], false, "{health}");
    // Latched: further renewals refuse without host contact, so the
    // forged third 204 is never even attempted.
    assert!(ctl.renew().is_err());
    peer.request();
    peer.request();
    assert!(
        peer.requests
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "fenced provider contacted host again"
    );
}

/// CAD-702: blips only buy time until the deadline. Six seconds with
/// no 204 fences permanently — and the fence arrives without another
/// host attempt once the deadline has passed.
#[test]
fn cad702_deadline_without_204_fences() {
    let peer = Peer::new(
        vec![reply(204, "", ""), reply(500, "", "overloaded")],
        Duration::ZERO,
    );
    let ttl = Duration::from_millis(400);
    let provider = HttpProvider::new(peer.url.clone(), ttl);
    let admitted = provider.acquire("h", 0).unwrap();
    // Inside the deadline the 500 is a blip, not a fence.
    assert!(provider.renew(&admitted).is_err());
    // Past the deadline with no 204: permanent loss, fenced without
    // spending another host attempt on a lease already lapsed.
    thread::sleep(ttl + Duration::from_millis(200));
    let err = provider.renew(&admitted).unwrap_err();
    assert!(
        err.to_string().contains("deadline expired"),
        "blips past the deadline must fence: {err}"
    );
    assert!(provider.renew(&admitted).is_err());
    peer.request();
    peer.request();
    assert!(
        peer.requests
            .recv_timeout(Duration::from_millis(100))
            .is_err(),
        "deadline-fenced provider contacted host again"
    );
}

/// CAD-702: a renewal that stalls past its attempt budget fails that
/// attempt only — bounded at two seconds, retryable while the
/// deadline is open, healable by the next 204.
#[test]
fn cad702_stalled_renewal_retries_within_budget() {
    let peer = Peer::staggered(vec![
        (reply(204, "", ""), Duration::ZERO),
        (reply(204, "", ""), Duration::from_secs(4)),
        (reply(204, "", ""), Duration::ZERO),
    ]);
    let dir = tempfile::TempDir::new().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let ctl = start_lease(
        &state,
        Arc::new(peer.provider()),
        HOST_RENEW_URL.into(),
        Duration::from_secs(2),
        Duration::from_secs(10),
    )
    .unwrap();
    let started = Instant::now();
    let err = ctl.renew().unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "renewal attempt was not bounded at two seconds: {:?}",
        started.elapsed()
    );
    assert!(
        err.to_string().contains("timed out") || err.to_string().contains("transport"),
        "stalled attempt misreported: {err}"
    );
    assert!(
        !ctl.fence().tripped(),
        "a stalled attempt fenced the daemon"
    );
    assert_eq!(ctl.status_json()["retrying"], true);
    // The stalled attempt's 204 arrives far too late to count — but
    // the next renewal heals, proving the stall never latched.
    ctl.renew().unwrap();
    assert!(!ctl.fence().tripped());
    assert_eq!(ctl.status_json()["retrying"], false);
}
