//! CAD-1159 INDEPENDENT request-bound check (check author session only).
//! Synthetic UNAUTHENTICATED `UnixStream::pair` transport ONLY: this is a
//! local cfg(test) `Channel` that never runs `connect()`, so no endpoint,
//! peer-credential, kernel-custody or Root-authority semantics are involved.
//! It checks the finite-bytes request guard before any socket write, and the
//! unchanged <=8MiB response transport for an UNTRUSTED DTO — it asserts
//! nothing about actual authorization, signatures or custody. Limit 4096 is
//! literal per docs/CONSTRUCTOR-PI-WIRE.md "Requests max4096 bytes"; no
//! MAX_REQUEST constant exists upstream of this check.
use super::*;
use std::io::Read;
use std::io::Write;
use std::thread;

const REQUEST_LIMIT: usize = 4096;

fn selection() -> Selection {
    Selection {
        alias_sha256: "a".repeat(64),
        generation: "b".repeat(32),
        role: Role::Worker,
        model: "provider/model".into(),
    }
}

fn channel(peer: UnixStream) -> Channel {
    Channel {
        stream: peer,
        until: Instant::now() + Duration::from_millis(100),
        consumed: false,
    }
}

/// Exact-size request via serialized JSON length, never source text.
fn provision_at(limit: usize) -> Request {
    let fixed = |alias: &str| Request::Provision {
        version: 1,
        selection: selection(),
        alias: alias.into(),
    };
    let base = serde_json::to_vec(&fixed("")).unwrap().len();
    fixed(&"d".repeat(limit - base))
}

/// Consume and Retire burn BEFORE any send. Sized by serialized JSON length.
fn burned_requests(size: usize) -> [Request; 2] {
    let fixed = |operation: String| {
        [
            Request::Consume {
                version: 1,
                selection: selection(),
                operation: operation.clone(),
            },
            Request::Retire {
                version: 1,
                selection: selection(),
                operation,
            },
        ]
    };
    let [consume, _] = fixed(String::new());
    let base = serde_json::to_vec(&consume).unwrap().len();
    fixed("c".repeat(size - base))
}

/// Stable nonblocking read via &UnixStream: empty peer (WouldBlock) proves NO
/// bytes were written by exchange(); any byte returned proves a write escaped.
fn observed(stream: &UnixStream) -> usize {
    let mut byte = [0u8; 1];
    let mut readable = stream;
    match readable.read(&mut byte) {
        Ok(n) => n,
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => 0,
        Err(e) => panic!("peer read: {e}"),
    }
}

fn serve_echo(stream: &mut UnixStream) -> Option<u32> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).ok()?;
    let len = u32::from_be_bytes(len);
    let mut frame = vec![0u8; len as usize];
    stream.read_exact(&mut frame).ok()?;
    let response = Response::Refused;
    let body = serde_json::to_vec(&response).unwrap();
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .and_then(|()| stream.write_all(&body))
        .unwrap();
    Some(len)
}

/// Transport-only UNTRUSTED Response DTO serialized >4096 and <8MiB. This is
/// synthetic shape filler for the byte guard; it is NOT an actual Authorized
/// launch, signature proof or accepted profile, and nothing validates it.
fn oversized_response() -> Response {
    let mut launch = Authorized {
        version: 1,
        selection: selection(),
        alias: "synthetic-transport-filler".into(),
        operation: "c".repeat(32),
        supervisor: SUPERVISOR_UID,
        guest: GUEST_UID,
        guest_gid: 21002,
        shared_gid: 21003,
        image: ImageProfile {
            helper_sha256: [1; 32],
            node_sha256: [2; 32],
            cli: "cli".into(),
            extensions: vec!["ext".into()],
            files: vec![GraphFile {
                path: "node".into(),
                size: 1,
                mode: 0o755,
                sha256: [2; 32],
            }],
        },
    };
    let fixed = |launch: Authorized, bytes: usize| Response::Authorized {
        launch,
        signed: Box::new(SignedOperation {
            authorization: "e".repeat(bytes),
            scope: OperationScope {
                version: 1,
                binding_json: String::new(),
                global: String::new(),
                company: String::new(),
                epoch: 0,
                lineage: String::new(),
                database_epoch: 0,
                alias: "synthetic-transport-filler".into(),
                selection: selection(),
                helper_sha256: [1; 32],
                node_sha256: [2; 32],
                profile_sha256: [3; 32],
                policy_sha256: [4; 32],
            },
        }),
    };
    let empty = serde_json::to_vec(&fixed(launch.clone(), 0)).unwrap().len();
    let body = empty + 5000;
    assert!(body > REQUEST_LIMIT && body < MAX_FRAME);
    launch.alias.push('x');
    fixed(launch, 5000)
}

/// RED on old code: 4097-byte request used to be fully written then error on
/// read timeout. The guard must Err BEFORE any byte reaches the peer.
#[test]
fn oversized_request_writes_nothing() {
    let request = provision_at(REQUEST_LIMIT + 1);
    assert_eq!(
        serde_json::to_vec(&request).unwrap().len(),
        REQUEST_LIMIT + 1
    );
    let (peer, mine) = UnixStream::pair().unwrap();
    peer.set_nonblocking(true).unwrap();
    let mut ch = channel(mine);
    assert!(ch.exchange(&request).is_err());
    assert_eq!(
        observed(&peer),
        0,
        "oversized request must not write even the length prefix"
    );
}

/// Consume and Retire burn BEFORE any send: each oversized request still sets
/// consumed, sends nothing, and a second send stays refused with zero bytes.
#[test]
fn oversized_consume_and_retire_burn_without_send() {
    for request in burned_requests(REQUEST_LIMIT + 64) {
        assert!(serde_json::to_vec(&request).unwrap().len() > REQUEST_LIMIT);
        let (peer, mine) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut ch = channel(mine);
        assert!(ch.exchange(&request).is_err());
        assert!(ch.consumed, "burned before send remains sticky");
        assert_eq!(observed(&peer), 0);
        // Second send attempt on a burned channel is refused, still zero bytes.
        let small = provision_at(REQUEST_LIMIT);
        assert!(ch.exchange(&small).is_err());
        assert_eq!(observed(&peer), 0);
    }
}

/// Legitimate request at exactly the limit is written and the small response
/// echo deserializes: positive smoke for the request write + response read.
#[test]
fn in_limit_request_round_trips() {
    let request = provision_at(REQUEST_LIMIT);
    assert_eq!(serde_json::to_vec(&request).unwrap().len(), REQUEST_LIMIT);
    let (mut peer, mine) = UnixStream::pair().unwrap();
    let echo = thread::spawn(move || serve_echo(&mut peer));
    let mut ch = channel(mine);
    let response = ch.exchange(&request).unwrap();
    assert!(matches!(response, Response::Refused));
    assert_eq!(echo.join().unwrap(), Some(REQUEST_LIMIT as u32));
}

/// Response transport stays MAX_FRAME-bounded: a valid UNTRUSTED Response DTO
/// serialized >4096 and <8MiB is written by the synthetic peer and read back
/// intact. Transport shape only — no authority or signature claim.
#[test]
fn oversized_response_round_trips_under_max_frame() {
    let request = provision_at(REQUEST_LIMIT);
    let response = oversized_response();
    let body = serde_json::to_vec(&response).unwrap();
    assert!(body.len() > REQUEST_LIMIT && body.len() < MAX_FRAME);
    let (mut peer, mine) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        let mut len = [0u8; 4];
        peer.read_exact(&mut len).unwrap();
        let mut frame = vec![0u8; u32::from_be_bytes(len) as usize];
        peer.read_exact(&mut frame).unwrap();
        peer.write_all(&(body.len() as u32).to_be_bytes())
            .and_then(|()| peer.write_all(&body))
            .unwrap();
    });
    let mut ch = channel(mine);
    let got = ch.exchange(&request).unwrap();
    writer.join().unwrap();
    assert!(matches!(got, Response::Authorized { .. }));
}
