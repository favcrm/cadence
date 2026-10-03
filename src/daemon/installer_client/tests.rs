//! Ordinary-UID socket mechanics, never production authority or root eligibility.
use super::*;
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::thread;

const OP: &str = "11111111-1111-4111-8111-111111111111";
const INSTALLER_GEN: &str = "22222222222222222222222222222222";
const RECIPIENT_GEN: &str = "33333333333333333333333333333333";
fn line(action: Action) -> String {
    let verb = action.verb();
    let envelope = if action == Action::Install {
        " a.b.c"
    } else {
        ""
    };
    format!("{verb} {OP} {INSTALLER_GEN} {RECIPIENT_GEN}{envelope}")
}
fn enrolled() -> Enrollment {
    Enrollment {
        pid: std::process::id(),
        starttime: crate::peer::proc_starttime(std::process::id()).unwrap(),
        generation: RECIPIENT_GEN.into(),
        digest: [1; 32],
    }
}
fn read_line(stream: &mut UnixStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut out = Vec::new();
    let mut byte = [0];
    while stream.read_exact(&mut byte).is_ok() {
        out.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    out
}

/// Real named socket, real nonblocking connect/write/read. Injecting an endpoint
/// here does NOT invoke or bypass `from_host_stdin`'s production factories.
fn transport_case(
    action: Action,
    response: Vec<u8>,
    chunks: bool,
    delay: Duration,
    budget: Duration,
) -> Result<TransportEvidence> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grant.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let expected = format!("{}\n", line(action)).into_bytes();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert_eq!(read_line(&mut stream), expected);
        thread::sleep(delay);
        if chunks {
            for chunk in response.chunks(3) {
                if stream.write_all(chunk).is_err() {
                    break;
                }
            }
        } else {
            let _ = stream.write_all(&response);
        }
    });
    let deadline = Deadline::new(budget);
    let result = (|| {
        let stream = connect(&path, deadline)?;
        let text = line(action);
        let request = Request::parse(&text)?;
        write_frame(&stream, &request.frame(), deadline)?;
        let response = read_response(&stream, deadline)?;
        classify(&request, &response, &enrolled())
    })();
    server.join().unwrap();
    result
}

#[test]
fn named_socket_partial_ack_and_exact_correlation() {
    for action in [Action::Challenge, Action::Install] {
        let text = line(action);
        let request = Request::parse(&text).unwrap();
        let e = enrolled();
        let ack = request.acknowledgement(e.pid, e.starttime).into_bytes();
        let got =
            transport_case(action, ack, true, Duration::ZERO, Duration::from_secs(2)).unwrap();
        assert_eq!(
            got,
            if action == Action::Challenge {
                TransportEvidence::Recipient
            } else {
                TransportEvidence::ConsumedAcknowledgement
            }
        );
    }
}

#[test]
fn lost_truncated_malformed_oversized_and_uncorrelated_ack_unknown() {
    let text = line(Action::Install);
    let request = Request::parse(&text).unwrap();
    let good = request.acknowledgement(1, 1);
    let responses = [
        Vec::new(),
        good.trim_end_matches('\n').as_bytes().to_vec(),
        b"ok consumed\n".to_vec(), // old unbound response is never accepted
        good.replace(OP, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            .into_bytes(),
        good.replace(INSTALLER_GEN, "44444444444444444444444444444444")
            .into_bytes(),
        good.replace(RECIPIENT_GEN, "55555555555555555555555555555555")
            .into_bytes(),
        format!("{good}{good}").into_bytes(),
        vec![b'x'; MAX_FRAME + 1],
        b"err peer asserted success\n".to_vec(),
        vec![0xff, b'\n'],
    ];
    for response in responses {
        assert!(matches!(
            transport_case(
                Action::Install,
                response,
                false,
                Duration::ZERO,
                Duration::from_secs(2)
            ),
            Err(Error::OutcomeUnknown(_))
        ));
    }
}

#[test]
fn challenge_pid_and_starttime_must_equal_measured_enrollment() {
    let text = line(Action::Challenge);
    let request = Request::parse(&text).unwrap();
    let e = enrolled();
    for ack in [
        request.acknowledgement(e.pid + 1, e.starttime),
        request.acknowledgement(e.pid, e.starttime + 1),
    ] {
        assert!(transport_case(
            Action::Challenge,
            ack.into_bytes(),
            false,
            Duration::ZERO,
            Duration::from_secs(2)
        )
        .is_err());
    }
}

#[test]
fn one_absolute_deadline_refuses_late_and_unfinished_frames() {
    let text = line(Action::Install);
    let request = Request::parse(&text).unwrap();
    assert!(transport_case(
        Action::Install,
        request.acknowledgement(1, 1).into_bytes(),
        false,
        Duration::from_millis(150),
        Duration::from_millis(50)
    )
    .is_err());
    let (client, mut server) = UnixStream::pair().unwrap();
    let writer = thread::spawn(move || {
        for _ in 0..10 {
            if server.write_all(b"o").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
    });
    let start = Instant::now();
    assert!(read_response(&client, Deadline::new(Duration::from_millis(70))).is_err());
    assert!(start.elapsed() < Duration::from_millis(200));
    drop(client);
    writer.join().unwrap();
}

#[test]
fn request_size_counts_all_framing_and_capsule_receipt() {
    let prefix = format!("install-v1 {OP} {INSTALLER_GEN} {RECIPIENT_GEN} ");
    let exact = format!("{}{}", prefix, "a".repeat(MAX_FRAME - prefix.len() - 1));
    let request = Request::parse(&exact).unwrap();
    assert_eq!(request.frame().len(), MAX_FRAME);
    assert!(Request::parse(&(exact.clone() + "a")).is_err());
    let (mut host, client) = UnixStream::pair().unwrap();
    let sender = thread::spawn(move || {
        let _ = host.write_all(format!("{exact} a.b.c\n").as_bytes());
    });
    assert!(read_stdin(client.as_raw_fd(), Deadline::new(Duration::from_secs(2))).is_err());
    drop(client);
    sender.join().unwrap();
}

#[test]
fn stdin_partial_capsule_eof_and_closed_schema() {
    for text in [
        format!("{}\n", line(Action::Challenge)),
        format!("{} a.b.c\n", line(Action::Install)),
    ] {
        let (mut host, client) = UnixStream::pair().unwrap();
        let sender = thread::spawn(move || {
            for part in text.as_bytes().chunks(5) {
                host.write_all(part).unwrap();
            }
        });
        let capsule =
            read_stdin(client.as_raw_fd(), Deadline::new(Duration::from_secs(2))).unwrap();
        capsule.parts().unwrap();
        sender.join().unwrap();
    }
    for text in [
        line(Action::Challenge),
        format!("{}\nextra\n", line(Action::Challenge)),
        format!("{} \n", line(Action::Challenge)),
        format!("{}\r\n", line(Action::Challenge)),
        format!("{}\n", line(Action::Install)),
        format!("{} a.b.c extra\n", line(Action::Install)),
    ] {
        let (mut host, client) = UnixStream::pair().unwrap();
        let sender = thread::spawn(move || {
            host.write_all(text.as_bytes()).unwrap();
        });
        assert!(read_stdin(client.as_raw_fd(), Deadline::new(Duration::from_secs(2))).is_err());
        sender.join().unwrap();
    }
}

#[test]
fn blocked_stdin_and_nonreading_peer_share_finite_budget() {
    let (_host, client) = UnixStream::pair().unwrap();
    assert!(read_stdin(client.as_raw_fd(), Deadline::new(Duration::from_millis(30))).is_err());
    let (client, _server) = UnixStream::pair().unwrap();
    let size: libc::c_int = 1024;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                client.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    let mut frame = vec![b'a'; MAX_FRAME];
    *frame.last_mut().unwrap() = b'\n';
    assert!(write_frame(&client, &frame, Deadline::new(Duration::from_millis(30))).is_err());
    // No new budget at write/read: once used, the same deadline stays elapsed.
    let deadline = Deadline::new(Duration::from_millis(10));
    thread::sleep(Duration::from_millis(20));
    assert!(write_frame(&client, b"a\n", deadline).is_err());
    assert!(read_response(&client, deadline).is_err());
}

fn policy(root: &str) -> Vec<(&str, u32, u32, u32)> {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    vec![
        (root, uid, gid, libc::S_IFDIR | 0o700),
        ("grant.sock", uid, gid, libc::S_IFSOCK | 0o600),
    ]
}
#[test]
fn held_topology_connect_rechecks_leaf_and_parent_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("parent");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = path.join("grant.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let p = policy(path.to_str().unwrap());
    let topology = Topology::capture(&p).unwrap();
    let _stream = topology
        .connect(Deadline::new(Duration::from_secs(2)))
        .unwrap();
    let (_peer, _) = listener.accept().unwrap();
    topology.recheck(&p).unwrap();
    std::fs::rename(&socket, path.join("old.sock")).unwrap();
    let _replacement = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(topology.recheck(&p).is_err());
    let topology = Topology::capture(&p).unwrap();
    std::fs::rename(&path, dir.path().join("old-parent")).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let _replacement = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(topology.recheck(&p).is_err());
}

#[test]
fn topology_refuses_symlink_wrong_owner_and_writable_parent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("grant.sock");
    let _listener = UnixListener::bind(&socket).unwrap();
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
    let p = policy(dir.path().to_str().unwrap());
    assert!(Topology::capture(&p).is_ok());
    let mut wrong_owner = p.clone();
    wrong_owner[1].1 += 1;
    assert!(Topology::capture(&wrong_owner).is_err());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o720)).unwrap();
    assert!(Topology::capture(&p).is_err());
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::rename(&socket, dir.path().join("real.sock")).unwrap();
    symlink("real.sock", &socket).unwrap();
    assert!(Topology::capture(&p).is_err());
    let alias = dir.path().join("alias");
    symlink(dir.path(), &alias).unwrap();
    assert!(Topology::capture(&policy(alias.to_str().unwrap())).is_err());
}

#[test]
fn kernel_peer_and_enrolled_process_refuse_caller_observations() {
    let dir = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(dir.path().join("grant.sock")).unwrap();
    let client = connect(
        &dir.path().join("grant.sock"),
        Deadline::new(Duration::from_secs(2)),
    )
    .unwrap();
    let (_server, _) = listener.accept().unwrap();
    // This ordinary UID socket is real. No supplied generation/digest can
    // turn it into the enrolled uid/gid-21000 supervisor.
    let mut e = enrolled();
    assert!(admit_supervisor(&client, &e, RECIPIENT_GEN).is_err());
    assert!(admit_self(&e, RECIPIENT_GEN).is_err());
    e.pid += 1;
    assert!(measured_process(std::process::id(), &e, RECIPIENT_GEN).is_err());
    e.pid = std::process::id();
    e.starttime += 1;
    assert!(measured_process(e.pid, &e, RECIPIENT_GEN).is_err());
    e.starttime -= 1;
    assert!(measured_process(e.pid, &e, RECIPIENT_GEN).is_err()); // real held exe custody/digest check
    assert!(measured_process(e.pid, &e, INSTALLER_GEN).is_err());
    e.digest = [0; 32];
    assert!(measured_process(e.pid, &e, RECIPIENT_GEN).is_err());
    assert!(from_host_stdin().is_err()); // refuses before reading/blocking stdin
    assert!(production_admission().is_err());
    assert!(receipt::production_trust_set().is_err());
    assert!(grant::production_consume_factory().is_err());
    assert!(grant::GrantListener::listen_production().is_err());
}
