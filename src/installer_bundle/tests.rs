//! Safe ordinary-file/FD and parser outcomes; NEVER privileged drop/live observer.
use super::*;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
fn elf() -> Vec<u8> {
    let mut b = vec![0; 160];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    b[16..18].copy_from_slice(&2u16.to_le_bytes());
    b[18..20].copy_from_slice(&62u16.to_le_bytes());
    b[20..24].copy_from_slice(&1u32.to_le_bytes());
    b[32..40].copy_from_slice(&64u64.to_le_bytes());
    b[52..54].copy_from_slice(&64u16.to_le_bytes());
    b[54..56].copy_from_slice(&56u16.to_le_bytes());
    b[56..58].copy_from_slice(&1u16.to_le_bytes());
    b[64..68].copy_from_slice(&1u32.to_le_bytes());
    b[68..72].copy_from_slice(&5u32.to_le_bytes());
    b[96..104].copy_from_slice(&160u64.to_le_bytes());
    b[104..112].copy_from_slice(&160u64.to_le_bytes());
    b
}
fn artifact(dir: &std::path::Path, name: &str, bytes: &[u8]) -> File {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}
fn digest(file: &File) -> [u8; 32] {
    crate::adapter::pi_guest::execfd::sha256_fd(file, file.metadata().unwrap().len()).unwrap()
}
#[test]
fn ordinary_static_fd_measurement_is_offset_preserving_and_refuses_loader_scripts() {
    let dir = tempfile::tempdir().unwrap();
    let mut file = artifact(dir.path(), "static", &elf());
    let uid = file.metadata().unwrap().uid();
    let pin = digest(&file);
    file.seek(SeekFrom::Start(37)).unwrap();
    files::measure(&file, pin, uid, Deadline::new()).unwrap();
    assert_eq!(file.stream_position().unwrap(), 37);
    for (i, bytes) in [
        b"#!/bin/sh\n".to_vec(),
        {
            let mut b = elf();
            b[64..68].copy_from_slice(&3u32.to_le_bytes());
            b
        },
        {
            let mut b = elf();
            b[64..68].copy_from_slice(&2u32.to_le_bytes());
            b
        },
        {
            let mut b = elf();
            b[18..20].copy_from_slice(&183u16.to_le_bytes());
            b
        },
        {
            let mut b = elf();
            b[32..40].copy_from_slice(&u64::MAX.to_le_bytes());
            b
        },
    ]
    .iter()
    .enumerate()
    {
        let f = artifact(dir.path(), &i.to_string(), bytes);
        assert!(files::measure(&f, digest(&f), uid, Deadline::new()).is_err());
    }
    std::fs::set_permissions(
        dir.path().join("static"),
        std::fs::Permissions::from_mode(0o775),
    )
    .unwrap();
    assert!(files::measure(&file, pin, uid, Deadline::new()).is_err());
}
#[test]
fn two_identical_artifacts_cannot_substitute_the_held_inode_and_mutations_refuse() {
    let dir = tempfile::tempdir().unwrap();
    let first = artifact(dir.path(), "first", &elf());
    let second = artifact(dir.path(), "second", &elf());
    let selected = files::stamp(&first).unwrap();
    assert_eq!(digest(&first), digest(&second));
    files::correspond(&first, &selected).unwrap();
    assert!(files::correspond(&second, &selected).is_err());
    let pin = digest(&first);
    let uid = first.metadata().unwrap().uid();
    first.write_all_at(&[1], 159).unwrap();
    assert!(files::measure(&first, pin, uid, Deadline::new()).is_err());
    assert!(files::correspond(&first, &selected).is_err());
    std::fs::rename(dir.path().join("second"), dir.path().join("replacement")).unwrap();
    std::fs::rename(dir.path().join("first"), dir.path().join("old-first")).unwrap();
    std::fs::rename(dir.path().join("replacement"), dir.path().join("first")).unwrap();
    let replacement = File::open(dir.path().join("first")).unwrap();
    assert!(files::correspond(&replacement, &selected).is_err());
}
#[test]
fn waiting_frame_eof_trailing_overflow_and_deadline_are_not_release() {
    for bytes in [
        b"".to_vec(),
        b"partial".to_vec(),
        b"one\ntwo\n".to_vec(),
        vec![b'x'; 32769],
    ] {
        let (mut host, client) = UnixStream::pair().unwrap();
        let writer = std::thread::spawn(move || {
            let _ = host.write_all(&bytes);
        });
        assert!(waiting_frame(client.as_raw_fd(), Deadline::new()).is_err());
        drop(client);
        writer.join().unwrap();
    }
    let (mut host, client) = UnixStream::pair().unwrap();
    host.write_all(b"framing evidence only\n").unwrap();
    drop(host);
    assert!(waiting_frame(client.as_raw_fd(), Deadline::new()).is_ok());
    let (_host, client) = UnixStream::pair().unwrap();
    assert!(waiting_frame(
        client.as_raw_fd(),
        Deadline(Instant::now() + Duration::from_millis(20))
    )
    .is_err());
    assert!(production_image().is_err());
    assert!(production_construction().is_err());
}
#[test]
fn fixed_bundle_factories_do_not_accept_synthetic_perfect_artifacts() {
    // Source factory refusal, not a fake kernel/observer result or privileged run.
    assert!(production_image().is_err());
    assert!(production_construction().is_err());
    let _unelected_source_names = (CLIENT, CARRIER, OBSERVER);
    // Pure stat parser reuse; no actual live observer/process measurement.
    let s = format!("123 (comm with spaces) S {} 99", "0 ".repeat(18));
    assert_eq!(crate::peer::parse_proc_starttime(&s), Some(99));
}

// Coordinator-owned acceptance compiles unchanged inside this test module.
include!("coordinator_boundary.rs");
