//! Independent CAD-1759 refusal acceptance — cc13-rev-sol.
//!
//! One check exercises the real guard's nested caps, persistent pause,
//! untrusted pause-file reads, abuse detection and reset-lookup budget.
//! A counting executable supplies fresh success or an actual nonzero exit;
//! no replacement guard, cap counter, pause reader or reset parser is used.
//! Each scenario self-execs into its own test process because bind's state
//! is process-global. The parent checks every scenario, even on buggy base.
//!
//! Module: src/gh_guard/cad1759_guard_hardening_refusal_acceptance.rs,
//! registered under cfg(test) in src/gh_guard.rs. The implementer must
//! not edit or weaken this independently authored refusal check.

use super::*;
use std::os::unix::fs::PermissionsExt;

struct Rig {
    _root: tempfile::TempDir,
    state: PathBuf,
    gh: PathBuf,
    calls: PathBuf,
    reset: i64,
}

impl Rig {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        std::fs::create_dir(&state).unwrap();
        let gh = root.path().join("gh");
        let calls = root.path().join("calls");
        let reset = epoch() + 30 * 60;
        let reply = serde_json::json!({
            "resources": {"core": {"remaining": 0, "reset": reset}}
        });
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n\
                 case \"$*\" in\n\
                 read) printf '%s\\n' '{{\"state\":\"MERGED\"}}';;\n\
                 rate) echo 'API rate limit exceeded' >&2; exit 1;;\n\
                 abuse) echo 'You triggered an ABUSE DETECTION mechanism' >&2; exit 1;;\n\
                 'api rate_limit') printf '%s\\n' '{reply}';;\n\
                 *) echo 'unexpected fake gh arguments' >&2; exit 2;;\n\
                 esac\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        bind(&state);
        let rig = Self {
            _root: root,
            state,
            gh,
            calls,
            reset,
        };
        // A real successful start before the refusal proof prevents a
        // missing/broken executable from masquerading as a guarded refusal.
        rig.read_ok();
        assert_eq!(rig.starts(), ["read"]);
        std::fs::write(&rig.calls, "").unwrap();
        rig
    }

    fn poll(&self, arg: &str) -> Result<Output, String> {
        run(Command::new(&self.gh).arg(arg), Duration::from_secs(2))
    }

    fn read_ok(&self) {
        let out = self.poll("read").unwrap();
        assert!(out.status.success(), "fresh read failed: {out:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stdout).unwrap()["state"],
            "MERGED"
        );
    }

    fn capped(&self) {
        let before = self.starts();
        let error = self.poll("read").unwrap_err();
        assert!(error.contains("cap"), "not a cap refusal: {error}");
        assert_eq!(self.starts(), before, "refused read started a process");
    }

    fn starts(&self) -> Vec<String> {
        std::fs::read_to_string(&self.calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn disk_until(&self) -> i64 {
        serde_json::from_slice::<Value>(
            &std::fs::read(self.state.join("gh-rate-limit.json")).unwrap(),
        )
        .unwrap()["until"]
            .as_i64()
            .unwrap()
    }
}

// The bounds come from the ticket, not from the implementation's constants.
fn epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn nested_caps() {
    let rig = Rig::new();
    with_call_cap(3, || {
        rig.read_ok();
        with_call_cap(99, || {
            rig.read_ok();
            rig.read_ok();
            rig.capped();
        });
        rig.capped();
        assert_eq!(rig.starts(), ["read", "read", "read"]);
    });
    rig.read_ok();
    assert_eq!(rig.starts().len(), 4, "cap leaked out of its scope");

    // A smaller inner cap also spends the enclosing allowance; exiting it
    // must restore only the unspent part, not discard or replenish the cap.
    std::fs::write(&rig.calls, "").unwrap();
    with_call_cap(4, || {
        with_call_cap(1, || {
            rig.read_ok();
            rig.capped();
        });
        for _ in 0..3 {
            rig.read_ok();
        }
        rig.capped();
    });
    assert_eq!(rig.starts().len(), 4);
}

fn longest_pause() {
    let rig = Rig::new();
    let long = record_pause(&rig.state, Some(epoch() + 50 * 60));
    assert_eq!(rig.disk_until(), long);
    for shorter in [Some(epoch() + 90), None] {
        let reported = record_pause(&rig.state, shorter);
        assert_eq!(reported, long, "short reset replaced the longer pause");
        assert_eq!(rig.disk_until(), long, "longer disk pause was shortened");
        assert_eq!(paused_until(&rig.state), Some(long));
    }
    assert!(rig.poll("read").is_err(), "persisted pause allowed a read");
    assert!(rig.starts().is_empty(), "paused read spawned a process");
}

fn untrusted_pause_file() {
    let rig = Rig::new();
    let file = rig.state.join("gh-rate-limit.json");
    std::fs::write(&file, format!(r#"{{"until":{}}}"#, i64::MAX)).unwrap();
    let before = epoch();
    let until = paused_until(&rig.state).expect("future pause was ignored");
    assert!(until > before, "future pause must still refuse reads");
    assert!(until <= epoch() + 65 * 60, "forged reset was not clamped");
    assert!(
        !paused_note(until).is_empty(),
        "pause note was not renderable"
    );
    assert!(
        rig.poll("read").is_err(),
        "forged future pause allowed a read"
    );
    assert!(rig.starts().is_empty());

    // Unparseable entries may be ignored, but none may panic or produce
    // an unbounded deadline. Include invalid UTF-8 and non-object JSON.
    let corrupt: &[&[u8]] = &[
        b"{",
        b"null",
        b"[]",
        b"true",
        b"\xff",
        br#"{"until":"forever"}"#,
        br#"{"until":null}"#,
        br#"{"until":18446744073709551615}"#,
        br#"{"until":1e999}"#,
        br#"{"until":-9223372036854775808}"#,
    ];
    for bytes in corrupt {
        std::fs::write(&file, bytes).unwrap();
        let pause = paused_until(&rig.state);
        if let Some(until) = pause {
            assert!(until <= epoch() + 65 * 60, "corrupt deadline: {until}");
            assert!(!paused_note(until).is_empty());
        }
        let out = rig.poll("read");
        if pause.is_some() {
            assert!(out.is_err(), "active pause supplied a read result");
        } else {
            assert!(out.unwrap().status.success(), "bad file broke a fresh read");
        }
    }
}

fn abuse_is_failed_read() {
    let rig = Rig::new();
    let out = rig.poll("abuse").unwrap();
    assert!(
        !out.status.success(),
        "abuse failure became successful evidence"
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("ABUSE DETECTION"));
    let until = paused_until(&rig.state).expect("abuse detection did not pause");
    assert_eq!(until, rig.reset);
    assert_eq!(rig.starts(), ["abuse", "api rate_limit"]);
    assert!(
        rig.poll("read").is_err(),
        "abuse pause allowed a later read"
    );
    assert_eq!(rig.starts(), ["abuse", "api rate_limit"]);
}

fn lookup_cannot_exceed_cap() {
    let rig = Rig::new();
    let before = epoch();
    with_call_cap(1, || {
        let out = rig.poll("rate").unwrap();
        assert!(!out.status.success());
        assert_eq!(rig.starts(), ["rate"], "reset lookup escaped the cap");
        rig.capped();
    });
    let until = paused_until(&rig.state).expect("missing fallback pause");
    assert!((before + 60..=epoch() + 65 * 60).contains(&until));
    assert_eq!(until, rig.disk_until());
    assert!(until < rig.reset, "no-budget path used the reset lookup");
    assert!(rig.poll("read").is_err(), "fallback pause was not enforced");
    assert_eq!(rig.starts(), ["rate"]);
}

fn lookup_spends_allowance() {
    let rig = Rig::new();
    with_call_cap(2, || {
        let out = rig.poll("rate").unwrap();
        assert!(!out.status.success());
        assert_eq!(rig.starts(), ["rate", "api rate_limit"]);
        assert_eq!(paused_until(&rig.state), Some(rig.reset));
        assert_eq!(rig.disk_until(), rig.reset);
        // Remove only this synthetic pause so it cannot hide an unspent
        // allowance. The next start must be refused by the cap itself.
        std::fs::remove_file(rig.state.join("gh-rate-limit.json")).unwrap();
        rig.capped();
        assert_eq!(rig.starts(), ["rate", "api rate_limit"]);
    });
    rig.read_ok();
    assert_eq!(rig.starts(), ["rate", "api rate_limit", "read"]);
}

#[test]
fn cad1759_guard_hardening_refuses_bypasses() {
    const CHILD: &str = "CADENCE_CAD1759_GUARD_REFUSAL_CASE";
    let cases = [
        "nested-caps",
        "longest-pause",
        "untrusted-pause-file",
        "abuse-failed-read",
        "lookup-no-budget",
        "lookup-spends-budget",
    ];
    if let Ok(case) = std::env::var(CHILD) {
        match case.as_str() {
            "nested-caps" => nested_caps(),
            "longest-pause" => longest_pause(),
            "untrusted-pause-file" => untrusted_pause_file(),
            "abuse-failed-read" => abuse_is_failed_read(),
            "lookup-no-budget" => lookup_cannot_exceed_cap(),
            "lookup-spends-budget" => lookup_spends_allowance(),
            _ => panic!("unknown refusal scenario: {case}"),
        }
        println!("cad1759-{case}-proof-complete");
        return;
    }
    let exact = format!(
        "{}::cad1759_guard_hardening_refuses_bypasses",
        module_path!().split_once("::").unwrap().1
    );
    let mut failed = Vec::new();
    for case in cases {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &exact, "--nocapture", "--test-threads", "1"])
            .env(CHILD, case)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        println!("{case}: {}\n{stdout}", out.status);
        if !out.status.success() {
            println!("{}", String::from_utf8_lossy(&out.stderr));
        }
        if !out.status.success()
            || !stdout.contains("running 1 test")
            || !stdout.contains(&format!("cad1759-{case}-proof-complete"))
        {
            failed.push(case);
        }
    }
    assert!(
        failed.is_empty(),
        "CAD-1759 refusal scenarios failed: {failed:?}"
    );
}
