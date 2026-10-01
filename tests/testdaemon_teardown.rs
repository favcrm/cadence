//! CAD-972: a fixture daemon that never becomes healthy must tear down
//! within a bound. Before the fix, `wait_health` panicked and the
//! unwinding `Drop` joined a daemon thread nothing had told to stop —
//! the join (a futex wait) never returned and a test process hung ~20
//! minutes. Every check below runs under an outer bound so a regression
//! fails the test instead of hanging it.

#![allow(clippy::disallowed_methods)]

mod common;

use common::*;
use std::os::unix::net::UnixListener;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Run `f` on its own thread; `None` when it has not finished within
/// `bound` (the thread is left behind — the process exit reaps it).
fn within<T: Send + 'static>(bound: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(bound).ok()
}

fn panic_text(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// A daemon thread that is alive but never answers (slow start), and
/// ends only when told to.
fn silent_until_stopped(
    _state: std::path::PathBuf,
    stop: Arc<AtomicBool>,
) -> cadence_agent::Result<()> {
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn unhealthy_start_tears_down_within_the_bound() {
    let begin = Instant::now();
    let outcome = within(Duration::from_secs(20), || {
        catch_unwind(AssertUnwindSafe(|| {
            TestDaemon::start_stub_thread(Duration::from_millis(300), silent_until_stopped)
        }))
    })
    .expect("unhealthy-start teardown hung past the outer bound");
    let Err(panic) = outcome else {
        panic!("a daemon that never answers must fail the start");
    };
    let text = panic_text(panic);
    assert!(text.contains("did not become healthy within"), "{text}");
    assert!(
        begin.elapsed() < Duration::from_secs(15),
        "{:?}",
        begin.elapsed()
    );
}

/// Mutation: without the stop signal the join has nothing to wait for
/// — the outer bound must trip. This is the CAD-972 hang.
#[test]
fn mutant_without_the_stop_signal_hangs() {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = thread::spawn(move || silent_until_stopped(Default::default(), flag).unwrap());
    let stopper = Arc::clone(&stop);
    let mutant = Teardown {
        signal_stop: false,
        ..Teardown::DEFAULT
    };
    let done = within(Duration::from_secs(2), move || {
        stop_and_join(handle, Some(&stopper), mutant, "mutant daemon")
    });
    assert!(
        done.is_none(),
        "the mutant must hang; the guard is not what ends the join"
    );
    stop.store(true, Ordering::SeqCst); // release the leaked thread
                                        // The real policy ends the same thread's twin at once.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let handle = thread::spawn(move || silent_until_stopped(Default::default(), flag).unwrap());
    let joined = within(Duration::from_secs(5), move || {
        stop_and_join(handle, Some(&stop), Teardown::DEFAULT, "real daemon")
    });
    assert!(matches!(joined, Some(Joined::Finished)));
}

/// A thread that ignores stop: the deadline must detach it, loudly,
/// inside a bound. Mutation: with the deadline effectively removed the
/// same call outlasts the outer bound.
#[test]
fn join_deadline_detaches_a_thread_that_ignores_stop() {
    let release = Arc::new(AtomicBool::new(false));
    let hold = Arc::clone(&release);
    let handle = thread::spawn(move || {
        while !hold.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
        }
    });
    let stop = Arc::new(AtomicBool::new(false));
    let policy = Teardown {
        join_deadline: Duration::from_millis(400),
        ..Teardown::DEFAULT
    };
    let begin = Instant::now();
    let joined = within(Duration::from_secs(5), {
        let stop = Arc::clone(&stop);
        move || stop_and_join(handle, Some(&stop), policy, "wedged daemon")
    });
    assert!(
        matches!(joined, Some(Joined::Detached)),
        "deadline must detach"
    );
    assert!(begin.elapsed() >= Duration::from_millis(400));
    assert!(
        stop.load(Ordering::SeqCst),
        "stop is signalled before waiting"
    );

    // Mutant: no effective deadline — the outer bound trips.
    let hold = Arc::clone(&release);
    let handle = thread::spawn(move || {
        while !hold.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(10));
        }
    });
    let mutant = Teardown {
        join_deadline: Duration::from_secs(3600),
        ..Teardown::DEFAULT
    };
    let done = within(Duration::from_secs(2), move || {
        stop_and_join(handle, None, mutant, "mutant daemon")
    });
    assert!(
        done.is_none(),
        "without a deadline the join must outlast the bound"
    );
    release.store(true, Ordering::SeqCst); // release both leaked threads
}

/// A socket that accepts and never answers: the health wait and the
/// drop's shutdown call are read-bounded, so neither waits the 700 s
/// `client::rpc` default.
#[test]
fn a_daemon_that_accepts_but_never_answers_does_not_hold_the_fixture() {
    let outcome = within(Duration::from_secs(40), || {
        catch_unwind(AssertUnwindSafe(|| {
            TestDaemon::start_stub_thread(Duration::from_millis(500), |state, stop| {
                let listener = UnixListener::bind(state.join("cadence.sock"))?;
                listener.set_nonblocking(true)?;
                let mut held = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    if let Ok((conn, _)) = listener.accept() {
                        held.push(conn); // never read, never reply
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            })
        }))
    })
    .expect("a silent socket held the fixture past the outer bound");
    let text = panic_text(outcome.err().expect("never healthy"));
    assert!(text.contains("did not become healthy within"), "{text}");
}

/// The real daemon, given no time to start. The start usually panics
/// and `Drop` then runs during the unwind, where a wedge only detaches
/// (no second panic). So the check is on time: a detach waits the whole
/// injected join deadline, a clean join comes in well under it.
#[test]
fn real_daemon_start_that_gives_up_early_tears_down() {
    const DEADLINE: Duration = Duration::from_secs(20);
    let policy = Teardown {
        join_deadline: DEADLINE,
        ..Teardown::DEFAULT
    };
    for _ in 0..5 {
        let (done, took) = within(Duration::from_secs(90), move || {
            let begin = Instant::now();
            let done = with_teardown(policy, || {
                catch_unwind(AssertUnwindSafe(|| {
                    TestDaemon::start_opts_waiting(daemon_opts(), Duration::ZERO)
                }))
            });
            (done, begin.elapsed())
        })
        .expect("real-daemon teardown hung past the outer bound");
        assert!(
            took < DEADLINE,
            "teardown waited out the join deadline: {took:?}"
        );
        if let Err(e) = done {
            let text = panic_text(e);
            assert!(text.contains("did not become healthy"), "{text}");
        }
    }
}

/// An in-process daemon whose `shutdown` the caller rule refuses (an
/// agent pane) must still tear down quickly through its stop flag: the
/// operator-helper fallback is for `daemon run` processes only. Without
/// the skip, Drop runs the detached helper against a daemon that is
/// leaving and panics "never answered" after ~20 s.
#[test]
fn caller_rule_refused_shutdown_does_not_run_the_operator_fallback() {
    use std::io::{BufRead, BufReader, Write};
    let begin = Instant::now();
    within(Duration::from_secs(60), || {
        let d = TestDaemon::start_stub_thread(Duration::from_secs(5), |state, stop| {
            let l = UnixListener::bind(state.join("cadence.sock"))?;
            l.set_nonblocking(true)?;
            while !stop.load(Ordering::SeqCst) {
                if let Ok((mut c, _)) = l.accept() {
                    let mut line = String::new();
                    let _ = BufReader::new(&c).read_line(&mut line);
                    let reply = if line.contains("health") {
                        r#"{"ok":true,"result":{}}"#
                    } else {
                        r#"{"ok":false,"error":{"kind":"rejected","message":"refused by the caller rule"}}"#
                    };
                    let _ = writeln!(c, "{reply}");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Ok(())
        });
        drop(d);
    })
    .expect("Drop hung or panicked (operator fallback?) within the outer bound");
    assert!(
        begin.elapsed() < Duration::from_secs(15),
        "{:?}",
        begin.elapsed()
    );
}

/// A daemon that ignores stop must FAIL its owning test with
/// TEARDOWN WEDGED (not pass with a leaked thread), within a bound.
/// The 300 ms deadline is injected; production uses 60 s.
#[test]
fn a_wedged_daemon_fails_its_test_at_drop() {
    let release = Arc::new(AtomicBool::new(false));
    let hold = Arc::clone(&release);
    let policy = Teardown {
        join_deadline: Duration::from_millis(300),
        ..Teardown::DEFAULT
    };
    let outcome = within(Duration::from_secs(20), move || {
        with_teardown(policy, || {
            catch_unwind(AssertUnwindSafe(|| {
                let d =
                    TestDaemon::start_stub_thread(Duration::from_secs(5), move |state, _stop| {
                        // Healthy (answers once bound) but ignores stop.
                        let l = UnixListener::bind(state.join("cadence.sock"))?;
                        l.set_nonblocking(true)?;
                        while !hold.load(Ordering::SeqCst) {
                            if let Ok((mut c, _)) = l.accept() {
                                use std::io::{BufRead, Write};
                                let mut line = String::new();
                                let _ = std::io::BufReader::new(&c).read_line(&mut line);
                                let _ = writeln!(c, r#"{{"ok":true,"result":{{}}}}"#);
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Ok(())
                    });
                drop(d); // the test body ends here, daemon wedged
            }))
        })
    })
    .expect("wedged teardown hung past the outer bound");
    release.store(true, Ordering::SeqCst);
    let text = panic_text(outcome.expect_err("a wedged daemon must fail the test at drop"));
    assert!(text.contains("TEARDOWN WEDGED"), "{text}");
}
