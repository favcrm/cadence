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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/renew", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let halted = Arc::clone(&stop);
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut replies = responses.into_iter();
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
                        let deadline = Instant::now() + delay;
                        while Instant::now() < deadline && !halted.load(Ordering::SeqCst) {
                            thread::sleep(Duration::from_millis(5));
                        }
                        if let Some(response) = replies.next() {
                            let _ = socket.write_all(response.as_bytes());
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
        HttpProvider {
            url: self.url.clone(),
        }
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
