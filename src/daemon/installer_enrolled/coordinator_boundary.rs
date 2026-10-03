// Independently authored by the CAD1113 coordinator; integrate unchanged into
// src/daemon/installer_enrolled/tests.rs (which already imports super::*).
// Real production factories and release path, never test argv or fake authority.
// Fixture is public signed RECEIPT bytes used as two compact-shaped envelopes:
// the parser positive proves framing only, not a valid grant/signature pair.
// Missing production custody must refuse before either envelope confers effects.
fn coordinator_boundary_frame() -> Vec<u8> {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../fixtures/installer-enrollment-wire-v1.json"
    )).unwrap();
    let compact = fixture["vectors"][0]["envelope"].as_str().unwrap();
    let bytes = format!("enrolled-install-r3 {compact} {compact}\n").into_bytes();
    assert!(bytes.len() <= MAX_FRAME);
    assert!(Frame::parse(&bytes).is_ok(), "positive framing control");
    bytes
}

#[test]
fn coordinator_missing_production_custody_blocks_factories_and_actual_release_without_effects() {
    let frame = coordinator_boundary_frame();
    let before = effect_counts();
    assert!(production_release_authority().is_err());
    assert!(production_enrolled_receiver().is_err());
    assert!(release_host_frame(&frame).is_err());
    assert_eq!(effect_counts(), before,
        "missing closed custody must not connect, observe owner, consume, enroll, exec or launch");
}

#[test]
fn coordinator_actual_receiver_refuses_before_reading_queued_frame_or_effects() {
    use std::io::{Read, Write};
    let frame = coordinator_boundary_frame();
    let (mut sender, mut receiver) = std::os::unix::net::UnixStream::pair().unwrap();
    receiver.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
    sender.write_all(&frame).unwrap();
    sender.shutdown(std::net::Shutdown::Write).unwrap();
    let before = effect_counts();
    assert!(handle_enrolled_stream(&receiver).is_err());
    assert_eq!(effect_counts(), before,
        "production receiver with missing authority must have zero effects");
    // Actual kernel socket control: refusal was not EOF, malformed framing,
    // missing bytes, or test argv; the entire input remains unread and intact.
    let mut queued = vec![0u8; frame.len()];
    receiver.read_exact(&mut queued).expect("production guard consumed host input");
    assert_eq!(queued, frame);
}
