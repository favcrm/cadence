//! Independent CAD-1922 acceptance (PR #1478): the daemon itself refuses a
//! stale sandbox profile in its captured provider environment instead of
//! binding the host's `$HOME/.cadence/workspace`.
//!
//! Authored from the ticket by a reviewer who did not write the PR; the
//! implementer must not edit or weaken it. It constructs the real `Shared`
//! (the constructor that holds the `workspace_for_provider_env` call), so a
//! change to that call site is noticed, which a test of the home function
//! alone cannot do.

use crate::daemon::Shared;

#[test]
fn cad1922_accept_daemon_refuses_a_stale_sandbox_profile_and_binds_nothing() {
    let _lock = crate::home::CAD1922_ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let names = [
        "HOME",
        "CADENCE_HOME",
        "CADENCE_STATE_DIR",
        "CADENCE_PM_DIR",
        "CADENCE_PROFILE",
        "CADENCE_SANDBOX_ROOT",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
    ];
    let saved: Vec<_> = names.iter().map(|n| (*n, std::env::var_os(n))).collect();
    for n in names {
        std::env::remove_var(n);
    }
    let restore = scopeguard(saved);

    // The test seam confines state to `std::env::temp_dir()`, so the fixture
    // lives there too: a hard-coded /tmp only agrees with it when TMPDIR is /tmp.
    let dir = tempfile::Builder::new().prefix("c22d").tempdir().unwrap();
    let home = dir.path().join("home");
    let base = dir.path().join("box");
    let pm = dir.path().join("pm");
    let state = dir.path().join("state");
    std::fs::create_dir_all(home.join(".cadence")).unwrap();
    std::fs::create_dir_all(base.join("acc/state")).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    crate::issue::Pm::init(&pm).unwrap();
    std::env::set_var("HOME", &home);

    let opts = |profile: &str| {
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        opts.provider_env.set("HOME", home.to_str().unwrap());
        opts.provider_env.set("CADENCE_PROFILE", profile);
        opts.provider_env
            .set("CADENCE_SANDBOX_ROOT", base.to_str().unwrap());
        opts
    };

    // No marker under the base: the daemon must refuse, not fall back.
    let error = Shared::new(&state, &opts("sandbox:acc"))
        .err()
        .expect("a stale profile must stop the daemon")
        .to_string();
    assert!(error.contains("refusing to fall back"), "{error}");
    assert!(
        !home.join(".cadence/workspace").exists(),
        "nothing may create or bind the host workspace"
    );
    // A marker for another sandbox is no better.
    std::fs::write(base.join("acc/.cadence-sandbox"), r#"{"name":"other"}"#).unwrap();
    assert!(Shared::new(&state, &opts("sandbox:acc")).is_err());

    // Positive control: the same options without a profile start, so the
    // refusals above came from the profile guard and not the fixture.
    let plain = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    plain
        .provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    plain.provider_env.set("HOME", home.to_str().unwrap());
    let started = Shared::new(&state, &plain);
    assert!(started.is_ok(), "control: {:?}", started.err());
    drop(restore);
}

/// Restores the saved environment when dropped.
struct Restore(Vec<(&'static str, Option<std::ffi::OsString>)>);

fn scopeguard(saved: Vec<(&'static str, Option<std::ffi::OsString>)>) -> Restore {
    Restore(saved)
}

impl Drop for Restore {
    fn drop(&mut self) {
        for (name, value) in &self.0 {
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
        }
    }
}
