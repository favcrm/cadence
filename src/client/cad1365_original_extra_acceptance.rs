//! CAD-1365 independent acceptance: the original-transfer reader refuses
//! the first byte past the declared body without waiting for EOF or the
//! deadline. Owned by the independent acceptance lane; not product source.
//!
//! The real `rpc_app_media_original` runs unmodified over a real Unix
//! socket. Only the daemon peer is a mock: a bound listener on
//! `<state_dir>/cadence.sock` that writes a fixed, valid reply. The reply
//! socket stays open until the parent releases it after the client has
//! returned, so the request half-close cannot be mistaken for a reply EOF.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::rpc_app_media_original;
use crate::proto;

const BODY: &[u8] = b"\x89PNG\r\n\x1a\nCAD1365-ORIGINAL";
// Peer hold bound: the reply socket stays open at most this long.
const PEER_HOLD: Duration = Duration::from_secs(15);
// The client must refuse within this bound; a deadline wait is 60s.
const MAX_REFUSAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
enum Tail {
    /// Exact body, then the peer closes: the control case.
    Clean,
    /// Header, body and one extra byte in the same write, peer held open.
    Buffered,
    /// Header and body, then one extra byte after a pause, peer held open.
    Later,
}

fn valid_reply() -> Vec<u8> {
    let sha = format!("sha256:{:x}", Sha256::digest(BODY));
    let header = proto::ok(json!({
        "transfer": "app-media-original/1",
        "asset_id": "asset-1",
        "media_type": "image/png",
        "bytes": BODY.len(),
        "sha256": sha,
    }));
    let mut reply = format!("{header}\n").into_bytes();
    reply.extend_from_slice(BODY);
    reply
}

/// Serves one connection. The request is read to its newline only; the
/// client's half-close on the request side is not treated as a reply
/// close. For the bad cases the reply socket is held open until `release`
/// fires or `PEER_HOLD` elapses, whichever comes first.
fn serve(socket: &Path, tail: Tail, release: Receiver<()>) -> JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("bind fixture daemon socket");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while request.last() != Some(&b'\n') {
            match std::io::Read::read(&mut stream, &mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => break,
            }
        }
        let mut reply = valid_reply();
        match tail {
            Tail::Clean => {
                let _ = stream.write_all(&reply);
                let _ = stream.shutdown(Shutdown::Write);
            }
            Tail::Buffered => {
                reply.push(b'!');
                let _ = stream.write_all(&reply);
                let _ = release.recv_timeout(PEER_HOLD);
            }
            Tail::Later => {
                let _ = stream.write_all(&reply);
                thread::sleep(Duration::from_millis(200));
                let _ = stream.write_all(b"!");
                let _ = release.recv_timeout(PEER_HOLD);
            }
        }
    })
}

fn run(tail: Tail) -> (Result<(Value, Vec<u8>), String>, Duration) {
    let dir = tempfile::Builder::new()
        .prefix("c1365")
        .tempdir_in("/tmp")
        .expect("short state dir");
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let server = serve(&dir.path().join("cadence.sock"), tail, release_rx);
    let started = Instant::now();
    let got = rpc_app_media_original(dir.path(), json!({"asset_id": "asset-1"}))
        .map_err(|e| e.to_string());
    let took = started.elapsed();
    // Release the held reply socket only now that the client has returned.
    let _ = release_tx.send(());
    server.join().expect("peer thread");
    (got, took)
}

#[test]
fn original_reply_refuses_first_extra_byte_without_waiting_for_eof() {
    // Control: the exact declared body followed by a clean EOF is accepted,
    // so the refusal below cannot be a reader that rejects everything.
    let (control, _) = run(Tail::Clean);
    let (header, body) = control.expect("exact body with clean EOF must be accepted");
    assert_eq!(body, BODY);
    assert_eq!(header["asset_id"], "asset-1");

    for tail in [Tail::Buffered, Tail::Later] {
        let (got, took) = run(tail);
        let err = got.expect_err("a byte past the declared body must be refused");
        assert!(
            err.contains("extra bytes"),
            "refused for the wrong reason: {err}"
        );
        assert!(
            took < MAX_REFUSAL,
            "refused only after {took:?}: the reader waited for EOF or the deadline"
        );
    }
}
