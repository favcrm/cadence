use super::*;

/// CAD-385 acceptance 2: a pane row whose pid is now held by an
/// unrelated process — a real one, recorded with an EARLIER start
/// time than the process holding the pid now — maps the caller to
/// no alias in any derivation, answering exactly as when the row is
/// not registered at all. The same row with the matching start
/// time does name the caller's pane (the check is not blanket).

#[test]
fn cad385_reused_pane_pid_maps_no_alias_like_an_unregistered_process() {
    let tree = PaneTree::spawn();
    let start = crate::peer::proc_starttime(tree.pane_pid()).unwrap();

    let (bare_dir, bare) = shared();
    register(&bare, bare_dir.path(), "elsewhere");
    let unregistered = identity_answers(&bare, tree.caller);
    assert!(unregistered[0].contains("Ok(None)"), "{unregistered:?}");

    let (dir, stale) = shared();
    register(&stale, dir.path(), "elsewhere");
    record_pane(&stale, dir.path(), "pm", tree.pane_pid(), Some(start - 1));
    assert_eq!(identity_answers(&stale, tree.caller), unregistered);

    let (live_dir, live) = shared();
    register(&live, live_dir.path(), "elsewhere");
    record_pane(&live, live_dir.path(), "pm", tree.pane_pid(), Some(start));
    assert!(matches!(
        live.slot_identity(tree.caller).unwrap(),
        Some(SlotWho::Pane { ref lane, .. }) if lane == "pm"
    ));
    assert_eq!(
        live.derived_caller("elsewhere", tree.caller, "answer")
            .unwrap(),
        ("pm".to_string(), "agent")
    );
}

/// CAD-385 acceptance 3: a pane row with a pid but no recorded start
/// time (written before schema v14) fails closed — no derivation
/// names its alias or lets the caller pass as unregistered: slot and
/// caller identity refuse naming the remedy, and operator proof
/// still counts the row as a pane.

#[test]
fn cad385_pane_row_without_a_start_time_fails_closed_naming_the_remedy() {
    let tree = PaneTree::spawn();
    let (dir, shared) = shared();
    register(&shared, dir.path(), "elsewhere");
    record_pane(&shared, dir.path(), "pm", tree.pane_pid(), None);

    let slot = shared.slot_identity(tree.caller).err().unwrap().to_string();
    let caller = match shared.caller_identity(tree.caller) {
        Err(e) => e.to_string(),
        Ok(_) => panic!("a legacy row must not resolve a caller"),
    };
    let answer = shared
        .derived_caller("elsewhere", tree.caller, "answer")
        .unwrap_err()
        .to_string();
    for refusal in [&slot, &caller, &answer] {
        assert!(refusal.contains("'pm'"), "{refusal}");
        assert!(refusal.contains("process start time"), "{refusal}");
        assert!(refusal.contains("cadence daemon restart"), "{refusal}");
        assert!(refusal.contains("doctor --host"), "{refusal}");
    }
    let proof = shared.operator_evidence(tree.caller).unwrap_err();
    assert!(proof.contains("registered pane 'pm'"), "{proof}");
}

/// The unmatched-caller tail: a broken walk refuses only while the
/// target lives; a clean walk stamps `operator` solely on positive
/// terminal evidence — a detached caller is `unknown`, never
/// `operator`.

#[test]
fn unmatched_caller_is_fail_closed() {
    // Broken walk: refused while the target pane lives.
    assert!(unmatched_caller(false, true, false, "answer").is_err());
    assert!(unmatched_caller(false, true, true, "answer").is_err());
    // …and `unknown` once it is gone.
    assert_eq!(
        unmatched_caller(false, false, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
    // Clean walk + foreign pty → operator.
    assert_eq!(
        unmatched_caller(true, true, true, "answer").unwrap(),
        ("operator".to_string(), "operator")
    );
    // Clean walk + no terminal (a full setsid detach) → unknown.
    assert_eq!(
        unmatched_caller(true, true, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
    assert_eq!(
        unmatched_caller(true, false, false, "answer").unwrap(),
        ("unknown".to_string(), "unknown")
    );
}

/// CAD-2059 acceptance, written by the reviewer from the ticket. It drives
/// the real guard through `operator_evidence`, `connection_caller`,
/// `rollout::require_operator` and `tcp_peer_operator_proof`, so deleting any
/// of that wiring turns it red.
///
/// The guard looks at real process ancestry and at the store the process
/// calls its default, so each probe is this test binary re-executed with a
/// cleared environment, HOME under a temp root, and detached from whoever ran
/// the suite. That also makes the answer the same from a plain shell, CI or a
/// coding agent's session. A probe asserts its store is under that root before
/// it opens anything; the real default store is never opened.
#[cfg(target_os = "linux")]
mod cad2059_acceptance {
    use super::*;
    use crate::daemon::caller_rule::Who;
    use std::io::{BufRead, Read};
    use std::process::{Command, Stdio};

    const ROLE: &str = "CADENCE_CAD2059_ROLE";
    const MARKED: &str = "marked";
    const CLEAN: &str = "clean";
    const ROOT_PREFIX: &str = "/tmp/c2059-";
    const PASSED: &str = "cad2059-probe-passed";
    /// A marker the probe "daemon" itself carries in the marked role.
    const DAEMON_MARKER: &str = "CLAUDE_CODE_SESSION_ID";

    fn path_var() -> String {
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_string())
    }

    /// Runs the probe detached (`setsid -f`, so its ancestry is only itself
    /// and `timeout`) and returns everything it wrote. Reading to EOF waits on
    /// the probe's exit, and `timeout` bounds a hang into a loud failure.
    fn run_probe(role: &str, marker: Option<&str>) -> String {
        let root = tempfile::Builder::new()
            .prefix("c2059-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("home");
        let tmp = root.path().join("tmp");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&tmp).unwrap();
        let test = format!(
            "{}::probe",
            module_path!()
                .split_once("::")
                .map(|(_, rest)| rest)
                .unwrap()
        );
        let mut command = Command::new("sh");
        command
            .args(["-c", r#"exec setsid -f timeout 240 "$@" 2>&1"#, "sh"])
            .arg(std::env::current_exe().unwrap())
            .args([
                test.as_str(),
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env_clear()
            .env("PATH", path_var())
            .env("HOME", &home)
            .env("TMPDIR", &tmp)
            .env(ROLE, role)
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        if let Some(marker) = marker {
            command.env(marker, "acceptance");
        }
        let mut child = command.spawn().unwrap();
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        child.wait().unwrap();
        output
    }

    #[test]
    fn cad2059_a_marker_is_refused_on_the_default_store_but_never_the_daemon_itself() {
        let output = run_probe(MARKED, Some(DAEMON_MARKER));
        assert!(output.contains(PASSED), "marked probe failed:\n{output}");
    }

    #[test]
    fn cad2059_a_clean_caller_passes_and_a_daemon_descendant_is_still_refused() {
        let output = run_probe(CLEAN, None);
        assert!(output.contains(PASSED), "clean probe failed:\n{output}");
    }

    /// A process outside the probe's own tree. It blocks reading a pipe held
    /// here, so it never execs again (`/proc/<pid>/environ` is stable) and
    /// leaves nothing behind once this guard drops.
    struct Detached {
        pid: u32,
        _hold: std::process::ChildStdin,
    }

    /// `script` runs in a new session and prints the caller's pid. The wait
    /// returns once `setsid`'s parent has exited, so the caller has been
    /// reparented and the probe is no longer on its ancestry.
    fn detached(script: &str, env: &[(&str, &str)]) -> Detached {
        let mut command = Command::new("sh");
        command
            .args(["-c", r#"exec setsid -f sh -c "$1""#, "sh", script])
            .env_clear()
            .env("PATH", path_var())
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .stdout(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let hold = child.stdin.take().unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        child.wait().unwrap();
        Detached {
            pid: line.trim().parse().unwrap(),
            _hold: hold,
        }
    }

    const WAITER: &str = "echo $$; read _";
    /// The shell keeps the marker and stays on the ancestry; the caller it
    /// starts has scrubbed that marker from its own environment.
    const SCRUBBED_UNDER_MARKED_SHELL: &str = r#"env -u CLAUDECODE sh -c 'echo $$; read _'; :"#;

    fn board_proof(state: &std::path::Path) -> std::result::Result<(), String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(server).unwrap();
        let peer = client.local_addr().unwrap();
        // SAFETY: geteuid has no preconditions and cannot fail.
        let uid = unsafe { libc::geteuid() };
        crate::peer::tcp_peer_operator_proof(
            server,
            peer,
            uid,
            0,
            &crate::peer::AgentRoots::default(),
            state,
        )
    }

    fn who(shared: &Shared, pid: u32) -> Who {
        shared.connection_caller(pid).unwrap()
    }

    /// Only meaningful when re-executed by [`run_probe`].
    #[test]
    #[ignore = "re-executed with an isolated environment by the CAD-2059 acceptance tests"]
    fn probe() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        let default = crate::client::default_state_dir().unwrap();
        assert!(
            default.to_string_lossy().starts_with(ROOT_PREFIX),
            "refusing to open a store outside the temp root: {}",
            default.display()
        );
        std::fs::create_dir_all(&default).unwrap();
        let shared = Shared::new(&default, &ServeOptions::default()).unwrap();
        let me = std::process::id();

        // A real caller outside the daemon's tree with a clean ancestry is the
        // operator, whether or not the daemon carries a marker.
        let clean = detached(WAITER, &[]);
        shared.operator_evidence(clean.pid).unwrap();
        assert_eq!(who(&shared, clean.pid), Who::Operator);

        match role.as_str() {
            MARKED => {
                // The daemon's own pid passes though it carries a marker: its
                // internal dispatches come back over the socket.
                shared.operator_evidence(me).unwrap();
                assert_eq!(who(&shared, me), Who::Operator);

                // The same detached shape as `clean`, differing only by the
                // marker: every marker is refused, by name, on both entries.
                for marker in crate::peer::AGENT_RUNTIME_MARKERS {
                    let caller = detached(WAITER, &[(marker, "1")]);
                    let why = shared.operator_evidence(caller.pid).unwrap_err();
                    assert!(why.contains(marker), "{marker}: {why}");
                    assert!(
                        matches!(who(&shared, caller.pid), Who::Unproven(why) if why.contains(marker)),
                        "{marker} must be unproven"
                    );
                }

                // A marker on a hop refuses a caller that scrubbed its own.
                let scrubbed = detached(SCRUBBED_UNDER_MARKED_SHELL, &[("CLAUDECODE", "1")]);
                let why = shared.operator_evidence(scrubbed.pid).unwrap_err();
                assert!(why.contains("CLAUDECODE"), "{why}");

                // The rollout and board proofs run on this process, which has
                // no daemon behind it there: the marker refuses, by name.
                let why = crate::rollout::require_operator(&default, "acceptance")
                    .unwrap_err()
                    .to_string();
                assert!(why.contains(DAEMON_MARKER), "{why}");
                let why = board_proof(&default).unwrap_err();
                assert!(why.contains(DAEMON_MARKER), "{why}");

                // Another store is outside the guard, on all three entries.
                let other = tempfile::tempdir().unwrap();
                let other_shared = Shared::new(other.path(), &ServeOptions::default()).unwrap();
                let caller = detached(WAITER, &[("CLAUDECODE", "1")]);
                other_shared.operator_evidence(caller.pid).unwrap();
                crate::rollout::require_operator(other.path(), "acceptance").unwrap();
                board_proof(other.path()).unwrap();
            }
            CLEAN => {
                shared.operator_evidence(me).unwrap();
                crate::rollout::require_operator(&default, "acceptance").unwrap();
                board_proof(&default).unwrap();

                // A process the daemon launched is never the operator, clean
                // environment or not.
                let mut child = Command::new("sleep")
                    .arg("300")
                    .env_clear()
                    .env("PATH", path_var())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let why = shared.operator_evidence(child.id());
                let _ = child.kill();
                let _ = child.wait();
                let why = why.unwrap_err();
                assert!(why.contains("descends from the daemon"), "{why}");
            }
            other => panic!("unknown role {other}"),
        }
        println!("{PASSED}");
    }
}
