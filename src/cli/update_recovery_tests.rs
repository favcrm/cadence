//! Exercise the real updater host against a previous CLI's command
//! contract, rather than the UpdateHost mock's restart implementation.
use super::*;
use cadence_agent::test_seam::{self, Asserted};
use cadence_agent::update::{RestartOutcome, UpdateHost};
use std::os::unix::{fs::PermissionsExt, io::AsRawFd};

struct OldCli {
    dir: tempfile::TempDir,
    state: PathBuf,
    binary: PathBuf,
}

impl OldCli {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join("seam")).unwrap();
        std::fs::write(state.join("seam/token"), "cad628-fixture").unwrap();
        cadence_agent::store::Store::open(&state.join("cadence.sqlite3")).unwrap();
        let binary = dir.path().join("previous-cadence");
        // The old CLI can cold-start, but its restart performs a fleet
        // RPC first and fails when the replacement never reached its socket.
        std::fs::write(
            &binary,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
if [ "$3" = daemon ] && [ "$4" = restart ]; then
  echo 'Daemon is not reachable: old restart requires agent_list' >&2
  exit 1
fi
if [ "$3" = daemon ] && [ "$4" = start ]; then
  echo '{"status":"started"}'
  exit 0
fi
if [ "$3" = ui ]; then
  exit 0
fi
exit 2
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir, state, binary }
    }

    fn host(&self) -> RealUpdateHost<'_> {
        RealUpdateHost {
            state_dir: &self.state,
            layout: cadence_agent::upgrade::Layout {
                releases: self.dir.path().join("releases"),
                link: self.dir.path().join("cadence"),
            },
            source: cadence_agent::upgrade::Gh::new("unused/fixture"),
            label: "operator:cad628".into(),
            collect: Some(std::cell::RefCell::new(Vec::new())),
            pending: std::cell::RefCell::new(None),
            progress_log: None,
        }
    }

    fn claim(&self, holder: &str) {
        test_seam::scoped(Asserted::Operator, || {
            cadence_agent::rollout::claim(
                &self.state,
                &cadence_agent::rollout::ClaimRequest {
                    caller: &cadence_agent::rollout::Caller {
                        identity: holder.into(),
                        source: "as",
                    },
                    reason: "backward CLI recovery test",
                    target: None,
                    ttl: Duration::from_secs(600),
                    takeover: false,
                    now: cadence_agent::rollout::unix_now(),
                },
            )
            .unwrap();
        });
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls")).unwrap_or_default()
    }
}

#[test]
fn cad628_real_host_cold_starts_a_previous_cli_that_cannot_restart_offline() {
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    assert!(matches!(result, Ok(RestartOutcome::Clean)), "{result:?}");
    let calls = fixture.calls();
    assert!(
        calls.contains("daemon start --as operator:cad628"),
        "{calls}"
    );
    assert!(!calls.contains("daemon restart"), "{calls}");
}

#[test]
fn cad628_real_host_refuses_offline_start_without_the_matching_lease() {
    for holder in [None, Some("operator:other")] {
        let fixture = OldCli::new();
        if let Some(holder) = holder {
            fixture.claim(holder);
        }
        let result = test_seam::scoped(Asserted::Operator, || {
            fixture.host().restart(&fixture.binary)
        });
        assert!(result.is_err(), "{result:?}");
        assert!(
            fixture.calls().is_empty(),
            "a refused recovery executed the old CLI"
        );
    }
}

#[test]
fn cad628_real_host_refuses_agents_and_unproven_callers_with_forged_identity() {
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        let fixture = OldCli::new();
        fixture.claim("operator:cad628");
        let result = test_seam::scoped(who, || fixture.host().restart(&fixture.binary));
        assert!(result.is_err(), "forged operator label: {result:?}");
        assert!(fixture.calls().is_empty());
    }
}

#[test]
fn cad628_real_host_refuses_offline_recovery_over_restore_leftovers() {
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    std::fs::write(
        fixture
            .state
            .join("cadence.sqlite3.replaced-20260926T000000Z-deadbeef"),
        "previous store",
    )
    .unwrap();
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    assert!(result.is_err(), "{result:?}");
    assert!(fixture.calls().is_empty());
}

#[test]
fn cad628_real_host_refuses_an_unreachable_singleton_holder() {
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.state.join("cadence.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    assert!(result.is_err(), "{result:?}");
    assert!(fixture.calls().is_empty());
}

#[test]
fn cad628_real_host_preserves_semantic_rpc_refusal() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    let listener = UnixListener::bind(client::socket_path(&fixture.state)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = stop.clone();
    let server = std::thread::spawn(move || {
        while !server_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut request = String::new();
                    BufReader::new(&stream).read_line(&mut request).unwrap();
                    writeln!(
                        stream,
                        "{}",
                        cadence_agent::proto::err(&Error::rejected("fleet snapshot refused"))
                    )
                    .unwrap();
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("fixture socket: {error}"),
            }
        }
    });
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    stop.store(true, Ordering::SeqCst);
    server.join().unwrap();
    assert!(result.is_err(), "{result:?}");
    assert!(fixture.calls().is_empty());
}

#[test]
fn cad628_real_host_recreates_a_board_that_survived_the_failed_daemon() {
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    // The old CLI is a recorder; it never signals this fixture process.
    std::fs::write(fixture.state.join("ui.pid"), std::process::id().to_string()).unwrap();
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    assert!(matches!(result, Ok(RestartOutcome::Clean)), "{result:?}");
    let calls = fixture.calls();
    let actions: Vec<_> = calls.lines().collect();
    assert_eq!(actions.len(), 3, "{calls}");
    assert!(actions[0].contains("daemon start"), "{calls}");
    assert!(actions[1].contains("ui stop"), "{calls}");
    assert!(actions[2].contains("ui start"), "{calls}");
}

#[test]
fn cad628_real_host_reports_old_schema_refusal_without_restoring() {
    let fixture = OldCli::new();
    fixture.claim("operator:cad628");
    let backup = fixture.dir.path().join("backup.sqlite3");
    std::fs::write(&backup, "operator-owned pre-update backup").unwrap();
    std::fs::write(
        &fixture.binary,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/calls"
echo 'refusing: store schema 19 is newer than supported schema 18' >&2
exit 1
"#,
    )
    .unwrap();
    let result = test_seam::scoped(Asserted::Operator, || {
        fixture.host().restart(&fixture.binary)
    });
    assert!(
        matches!(result, Ok(RestartOutcome::Unclean(ref complaint)) if complaint.contains("store schema 19")),
        "{result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        "operator-owned pre-update backup"
    );
    assert_eq!(
        cadence_agent::rollout::store_schema(&fixture.state).unwrap(),
        Some(19)
    );
    assert_eq!(fixture.calls().lines().count(), 1);
    assert!(!fixture.calls().contains("restore"));
}
