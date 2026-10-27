//! CAD-1159 independently authored malformed runtime-frame DATA acceptance.
//! Body ownership: aos159-constructor-guard; implementers register only.
//! Actual socketpair/receiver and sticky framing state, no Root/runtime grant,
//! owned peer, signature, authenticated operation window or launch is supplied.
//! begin_operation below selects the codec for this transport-only check; it
//! does NOT establish the genuine runtime admission required by its caller.
use super::{Deadline, Duplex};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

#[test]
fn malformed_runtime_utf8_refuses_and_cannot_resume_on_a_valid_frame() {
    let (input, mut peer) = UnixStream::pair().unwrap();
    let deadline = Deadline::new();
    let mut channel = Duplex::new(input.as_raw_fd(), input.as_raw_fd(), deadline).unwrap();
    channel.begin_operation(deadline).unwrap();

    // Reach the actual runtime UTF-8 receiver, not an ASCII/cold/EOF refusal.
    // Prompt bytes are ordinary DATA and provide no native caller authority.
    let valid = serde_json::to_vec(&serde_json::json!({"prompt":"請總結 😀"})).unwrap();
    peer.write_all(&valid).unwrap();
    peer.write_all(b"\n").unwrap();
    assert_eq!(channel.receive().unwrap().0, valid);
    let before_frames = channel.frames;
    let before_total = channel.total;
    assert_eq!(before_frames, 1);

    // This short complete frame has no raw delimiter/CR/NUL inside it. Its
    // sole defect is invalid UTF-8, delivered by the real socket peer. Keep
    // that peer open and queue a valid successor: loss cannot become retry.
    let malformed = b"{\"prompt\":\"\xff\"}";
    assert!(malformed.len() < super::MAX_FRAME);
    assert!(!malformed.iter().any(|byte| b"\n\r\0".contains(byte)));
    peer.write_all(malformed).unwrap();
    peer.write_all(b"\n").unwrap();
    peer.write_all(&valid).unwrap();
    peer.write_all(b"\n").unwrap();

    assert!(channel.receive().is_err(), "malformed UTF-8 was admitted");
    deadline.check().unwrap();
    // Real pending bytes prove the complete bad frame reached framing, rather
    // than an unrelated readiness timeout or EOF providing a false positive.
    assert!(channel.pending.0.starts_with(malformed));
    assert_eq!(
        channel.pending.0.iter().position(|byte| *byte == b'\n'),
        Some(malformed.len())
    );
    assert!(channel.failed);
    assert_eq!(channel.frames, before_frames);
    assert_eq!(channel.total, before_total);

    assert!(
        channel.receive().is_err(),
        "queued valid frame resumed loss"
    );
    assert!(
        channel.begin_operation(deadline).is_err(),
        "new codec operation cleared fatal framing state"
    );
    assert!(
        channel.send(&valid).is_err(),
        "fatal channel still sent DATA"
    );
    assert_eq!(channel.frames, before_frames);
    assert_eq!(channel.total, before_total);
}
