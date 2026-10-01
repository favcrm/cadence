//! Wait for a nonblocking listener without delaying newly queued clients.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;

/// Readiness is advisory: the caller must still accept and handle its errors.
/// The timeout retains the outer loop's stop/closing observation interval.
pub(in crate::daemon) fn wait_for_connection(listener: &UnixListener) -> io::Result<bool> {
    wait_for(listener, 50)
}

/// The shared agent socket is optional; poll both descriptors with
/// one bounded wait so traffic on either wakes the accept loop.
pub(in crate::daemon) fn wait_for_connections(listeners: &[&UnixListener]) -> io::Result<bool> {
    if let [listener] = listeners {
        return wait_for_connection(listener);
    }
    let mut descriptors: Vec<libc::pollfd> = listeners
        .iter()
        .map(|listener| libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let result = unsafe { libc::poll(descriptors.as_mut_ptr(), descriptors.len() as _, 50) };
    if result < 0 {
        return poll_outcome(Err(io::Error::last_os_error()), 0);
    }
    if result == 0 {
        return Ok(false);
    }
    let mut ready = false;
    for descriptor in descriptors {
        if descriptor.revents != 0 {
            ready |= poll_outcome(Ok(1), descriptor.revents)?;
        }
    }
    Ok(ready)
}

fn wait_for(listener: &UnixListener, timeout_ms: i32) -> io::Result<bool> {
    let mut descriptor = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the borrowed listener keeps its fd alive; descriptor is one
    // initialized, writable pollfd for the duration of this bounded call.
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    let result = if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    };
    poll_outcome(result, descriptor.revents)
}

fn poll_outcome(result: io::Result<i32>, events: i16) -> io::Result<bool> {
    match result {
        // Return to the outer lifecycle checks instead of restarting the wait.
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(false),
        Err(error) => Err(error),
        Ok(0) => Ok(false),
        Ok(_) if events & libc::POLLNVAL != 0 => Err(io::Error::from_raw_os_error(libc::EBADF)),
        Ok(_) if events & (libc::POLLERR | libc::POLLHUP) != 0 => Err(io::Error::other(
            "daemon listener reported an error or hangup",
        )),
        Ok(_) if events & libc::POLLIN != 0 => Ok(true),
        Ok(_) => Err(io::Error::other(
            "daemon listener reported unexpected readiness",
        )),
    }
}

/// accept(2) failures that clear on their own: fd-table and kernel
/// memory pressure, and an interrupted syscall. The serve loop retries
/// these under a bounded backoff — an exit here abandons in-flight
/// turns to the next start's recovery sweep. Anything else is a dead
/// listener and stays fatal.
pub(in crate::daemon) fn transient_accept_error(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Interrupted {
        return true;
    }
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    fn listener() -> (tempfile::TempDir, UnixListener) {
        let root = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(root.path().join("accept.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        (root, listener)
    }

    fn assert_wakes(delayed: bool) {
        let (root, listener) = listener();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let queued =
            (!delayed).then(|| UnixStream::connect(root.path().join("accept.sock")).unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            started_tx.send(()).unwrap();
            // A long budget makes unconditional sleeping observable without
            // requiring a fragile sub-50ms production timing assertion.
            let ready = wait_for(&listener, 2_000);
            let accepted = listener.accept();
            done_tx.send((ready, accepted)).unwrap();
        });
        started_rx.recv().unwrap();
        let arriving = delayed.then(|| {
            thread::sleep(Duration::from_millis(50));
            UnixStream::connect(root.path().join("accept.sock")).unwrap()
        });
        let completed = done_rx.recv_timeout(Duration::from_secs(1));
        // Join our worker even when the completion assertion will fail.
        worker.join().unwrap();
        let (ready, accepted) = completed.expect("connection must interrupt the long idle wait");
        assert!(ready.unwrap());
        assert!(accepted.is_ok());
        drop((queued, arriving));
    }

    #[test]
    fn queued_connection_wakes_the_idle_listener() {
        assert_wakes(false);
    }

    #[test]
    fn arriving_connection_wakes_the_idle_listener() {
        assert_wakes(true);
    }

    #[test]
    fn second_listener_wakes_the_same_accept_loop() {
        let (a_root, a) = listener();
        let (b_root, b) = listener();
        let path = b_root.path().join("accept.sock");
        let (started_tx, started_rx) = mpsc::channel();
        let sender = thread::spawn(move || {
            started_tx.send(()).unwrap();
            UnixStream::connect(path)
        });
        started_rx.recv().unwrap();
        // wait_for_connections returns false on its own 50ms timeout by
        // design; the outer accept loop simply waits again. On a loaded host
        // the sender thread can be scheduled later than one timeout, so a
        // false is not a failure: only readiness on b ends the wait, and
        // readiness on a (never connected) or an error would fail the test.
        let deadline = Instant::now() + Duration::from_secs(30);
        let woke = loop {
            match wait_for_connections(&[&a, &b]) {
                Ok(false) if Instant::now() < deadline => continue,
                other => break other,
            }
        };
        // Join before asserting so a failure never leaves the sender behind,
        // and keep b_root alive until it has connected.
        let client = sender.join().unwrap();
        assert!(woke.unwrap(), "no readiness on the second listener");
        let _client = client.unwrap();
        assert!(b.accept().is_ok());
        assert_eq!(a.accept().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        drop((a_root, b_root));
    }

    #[test]
    fn empty_listener_times_out_without_readiness() {
        let (_root, listener) = listener();
        let start = Instant::now();
        assert!(!wait_for(&listener, 100).unwrap());
        // With no injected interruption, an empty listener must actually wait;
        // an immediate false result would turn the daemon's idle loop into a spin.
        assert!(start.elapsed() >= Duration::from_millis(75));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn interruption_returns_to_lifecycle_checks_and_other_errors_propagate() {
        assert!(!poll_outcome(Err(io::Error::from_raw_os_error(libc::EINTR)), 0).unwrap());
        assert_eq!(
            poll_outcome(Err(io::Error::from_raw_os_error(libc::ENOMEM)), 0)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::ENOMEM)
        );
    }

    #[test]
    fn transient_accept_errors_are_retried_and_permanent_ones_are_not() {
        for raw in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
            assert!(
                transient_accept_error(&io::Error::from_raw_os_error(raw)),
                "errno {raw} must be retried"
            );
        }
        assert!(transient_accept_error(&io::Error::from_raw_os_error(
            libc::EINTR
        )));
        for raw in [libc::EBADF, libc::EINVAL, libc::EPERM, libc::EFAULT] {
            assert!(
                !transient_accept_error(&io::Error::from_raw_os_error(raw)),
                "errno {raw} must stay fatal"
            );
        }
        // WouldBlock/ConnectionAborted take the caller's benign arm —
        // they are not retried here.
        assert!(!transient_accept_error(&io::Error::new(
            io::ErrorKind::WouldBlock,
            "idle"
        )));
    }

    #[test]
    fn error_readiness_cannot_become_a_busy_loop() {
        assert_eq!(
            poll_outcome(Ok(1), libc::POLLNVAL)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
        for events in [
            libc::POLLERR,
            libc::POLLHUP,
            libc::POLLIN | libc::POLLERR,
            libc::POLLIN | libc::POLLHUP,
            libc::POLLIN | libc::POLLNVAL,
            0,
        ] {
            assert!(poll_outcome(Ok(1), events).is_err());
        }
    }
}
