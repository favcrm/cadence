//! Bounded one-line frame transport for the private owner, operations and
//! attestation sockets. Every read is one newline-terminated document under an
//! absolute monotonic deadline; a slow trickle cannot hold the server and an
//! oversized peer is refused before the buffer grows.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Read exactly one newline-terminated frame, at most `max` bytes excluding
/// the newline, under a single absolute `budget`. Rejects a missing newline,
/// trailing bytes after it, an oversized line and an elapsed deadline.
pub(crate) fn read_frame(mut stream: &UnixStream, budget: Duration, max: usize) -> Result<Vec<u8>> {
    let deadline = Instant::now() + budget;
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(Error::rejected("frame deadline elapsed"));
        }
        stream
            .set_read_timeout(Some(deadline - now))
            .map_err(|e| Error::rejected(format!("frame read deadline failed: {e}")))?;
        let remaining = max + 1 - buf.len();
        if remaining == 0 {
            return Err(Error::rejected("frame exceeds the size bound"));
        }
        let want = std::cmp::min(remaining, chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Err(Error::rejected("frame closed without a newline")),
            Ok(n) => {
                let got = &chunk[..n];
                if let Some(pos) = got.iter().position(|&b| b == b'\n') {
                    if pos + 1 != got.len() {
                        return Err(Error::rejected("trailing bytes after frame"));
                    }
                    buf.extend_from_slice(&got[..pos]);
                    break;
                }
                buf.extend_from_slice(got);
            }
            Err(e) => return Err(Error::rejected(format!("frame read failed: {e}"))),
        }
    }
    if buf.len() > max {
        return Err(Error::rejected("frame exceeds the size bound"));
    }
    if buf.is_empty() {
        return Err(Error::rejected("empty frame"));
    }
    Ok(buf)
}

/// Write one newline-terminated frame under a bounded write deadline.
pub(crate) fn write_frame(mut stream: &UnixStream, body: &[u8], budget: Duration) -> Result<()> {
    stream
        .set_write_timeout(Some(budget))
        .map_err(|e| Error::rejected(format!("frame write deadline failed: {e}")))?;
    let mut out = Vec::with_capacity(body.len() + 1);
    out.extend_from_slice(body);
    out.push(b'\n');
    stream
        .write_all(&out)
        .map_err(|e| Error::rejected(format!("frame write failed: {e}")))
}

/// A success envelope: `{"ok":true,"data":<data>,"owner_epoch":<epoch>}`. The
/// epoch echo lets the authenticated supervisor client require the same live
/// owner epoch on every exchange; a changed/missing value invalidates.
pub(crate) fn ok_with_epoch(data: serde_json::Value, owner_epoch: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "ok": true,
        "data": data,
        "owner_epoch": owner_epoch
    }))
    .unwrap_or_else(|_| {
        br#"{"ok":false,"error":{"v":1,"code":"internal","message":"unavailable"}}"#.to_vec()
    })
}

/// A success envelope: `{"ok":true,"data":<data>}`.
pub(crate) fn ok(data: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"ok": true, "data": data})).unwrap_or_else(|_| {
        br#"{"ok":false,"error":{"v":1,"code":"internal","message":"unavailable"}}"#.to_vec()
    })
}

/// A closed-code error envelope with a fixed message.
pub(crate) fn err(code: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "ok": false,
        "error": {"v": 1, "code": code, "message": message}
    }))
    .unwrap_or_else(|_| {
        br#"{"ok":false,"error":{"v":1,"code":"internal","message":"unavailable"}}"#.to_vec()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    fn pair() -> (UnixStream, UnixStream) {
        UnixStream::pair().unwrap()
    }

    #[test]
    fn reads_one_bounded_line() {
        let (a, b) = pair();
        let mut w = &a;
        w.write_all(b"{\"v\":1}\n").unwrap();
        drop(a);
        let got = read_frame(&b, Duration::from_secs(1), 64).unwrap();
        assert_eq!(got, b"{\"v\":1}");
    }

    #[test]
    fn refuses_oversize_and_trailing() {
        let (a, b) = pair();
        let mut w = &a;
        w.write_all(&[b'x'; 80]).unwrap();
        w.write_all(b"\n").unwrap();
        drop(a);
        assert!(read_frame(&b, Duration::from_secs(1), 32).is_err());

        let (a, b) = pair();
        let mut w = &a;
        w.write_all(b"{} {}\n").unwrap();
        drop(a);
        assert!(read_frame(&b, Duration::from_secs(1), 64).is_err());
    }

    #[test]
    fn refuses_missing_newline_and_empty() {
        let (a, b) = pair();
        let mut w = &a;
        w.write_all(b"{}").unwrap();
        drop(a);
        assert!(read_frame(&b, Duration::from_secs(1), 64).is_err());

        let (a, b) = pair();
        let mut w = &a;
        w.write_all(b"\n").unwrap();
        drop(a);
        assert!(read_frame(&b, Duration::from_secs(1), 64).is_err());
    }
}
