//! CAD-584: the `CADENCE_HOME` resolver (`cadence_agent::home`).
//!
//! Env-mutating tests live in this binary alone — every case takes
//! `ENV_LOCK`, snapshots the five resolver inputs and restores them.
//! `old_*` fns are the pre-CAD-584 implementations copied verbatim
//! from `cd87ffe`: under the legacy layout the resolver must answer
//! byte-identically to what main resolved before this change.

// The e2e cases spawn the cadence binary directly — a test binary
// never runs the CAD-308 reaper, so its spawns need not register.
#![allow(clippy::disallowed_methods)]

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Mutex;

use cadence_agent::home;
use tempfile::TempDir;

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Every env var the resolver (or its legacy oracle) reads.
const VARS: [&str; 5] = [
    "CADENCE_HOME",
    "CADENCE_PM_DIR",
    "CADENCE_STATE_DIR",
    "XDG_STATE_HOME",
    "HOME",
];

/// Holds the lock for one test: clears all resolver inputs on
/// entry, restores their prior values on drop.
struct EnvGuard {
    prior: Vec<Option<OsString>>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl EnvGuard {
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = VARS.iter().map(std::env::var_os).collect();
        for k in VARS {
            std::env::remove_var(k);
        }
        Self { prior, _lock: lock }
    }
    fn set(&self, key: &'static str, value: impl AsRef<OsStr>) {
        std::env::set_var(key, value);
    }
    fn remove(&self, key: &'static str) {
        std::env::remove_var(key);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in VARS.iter().zip(self.prior.drain(..)) {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }
}

/// A `$HOME` under a short temp root — absolute, nothing cadence
/// owns inside it.
fn home_root() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().to_path_buf();
    (tmp, path)
}

fn mark_layout(home: &std::path::Path, content: &str) {
    let dir = home.join(".cadence");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(home::LAYOUT_MARKER), content).unwrap();
}

// ---------- the pre-CAD-584 resolvers, verbatim from cd87ffe ----------

/// `issue::default_dir` before CAD-584 (`src/issue/mod.rs`).
fn old_issue_default_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CADENCE_PM_DIR") {
        return PathBuf::from(dir);
    }
    old_home_default_dir()
}

/// `issue::home_default_dir` before CAD-584.
fn old_home_default_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .expect("HOME is not set to an absolute path");
    home.join("pm")
}

/// `client::state_dir` before CAD-584 (`src/client.rs`).
fn old_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CADENCE_STATE_DIR") {
        return PathBuf::from(dir);
    }
    old_default_state_dir()
}

/// `client::default_state_dir` before CAD-584.
fn old_default_state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(dir).join("cadence");
    }
    let home = std::env::var("HOME").expect("Cannot locate HOME for state directory");
    PathBuf::from(home).join(".local/state/cadence")
}

// ---------- precedence ----------

#[test]
fn cadence_home_env_selects_the_new_layout() {
    let _env = EnvGuard::new();
    _env.set("CADENCE_HOME", "/var/lib/cadence");
    let layout = home::layout().unwrap();
    assert_eq!(layout.source, home::Source::Env);
    assert_eq!(layout.root, PathBuf::from("/var/lib/cadence"));
    assert_eq!(home::home().unwrap(), PathBuf::from("/var/lib/cadence"));
    assert_eq!(
        home::tracker_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/tracker")
    );
    assert_eq!(
        home::state_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/state")
    );
    assert_eq!(
        home::vault_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/vault")
    );
    assert_eq!(
        home::repos_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/repos")
    );
}

#[test]
fn cadence_home_env_beats_the_layout_marker() {
    let _env = EnvGuard::new();
    let (_tmp, home_dir) = home_root();
    mark_layout(&home_dir, "1");
    _env.set("HOME", &home_dir);
    _env.set("CADENCE_HOME", "/srv/cadence");
    let layout = home::layout().unwrap();
    assert_eq!(layout.source, home::Source::Env);
    assert_eq!(layout.root, PathBuf::from("/srv/cadence"));
    // The marker lives at ~/.cadence, not the resolved root.
    assert!(!layout.marker_present);
}

/// REV-300: the marker is inert — a same-uid agent can plant
/// `~/.cadence/LAYOUT`, so it must never redirect resolution.
/// Without `CADENCE_HOME` every content resolves legacy; the flag
/// stays truthful only for `doctor --host` reporting.
#[test]
fn a_planted_layout_marker_stays_legacy() {
    for (content, declared) in [
        ("1", true),
        ("1\n", true),
        (" 1 ", true),
        ("2", false),
        ("", false),
        ("layout-1", false),
    ] {
        let _env = EnvGuard::new();
        let (_tmp, home_dir) = home_root();
        mark_layout(&home_dir, content);
        _env.set("HOME", &home_dir);
        let layout = home::layout().unwrap();
        assert_eq!(
            layout.source,
            home::Source::Legacy,
            "LAYOUT content {content:?}"
        );
        assert_eq!(
            layout.marker_present, declared,
            "LAYOUT content {content:?}"
        );
        assert_eq!(
            home::tracker_dir().unwrap(),
            old_issue_default_dir(),
            "LAYOUT content {content:?}"
        );
        assert_eq!(
            home::state_dir().unwrap(),
            old_state_dir(),
            "LAYOUT content {content:?}"
        );
        assert_eq!(
            home::vault_dir().unwrap(),
            old_issue_default_dir().join("wiki"),
            "LAYOUT content {content:?}"
        );
    }
}

#[test]
fn a_cadence_dir_without_a_marker_stays_legacy() {
    let _env = EnvGuard::new();
    let (_tmp, home_dir) = home_root();
    std::fs::create_dir_all(home_dir.join(".cadence")).unwrap();
    _env.set("HOME", &home_dir);
    let layout = home::layout().unwrap();
    assert_eq!(layout.source, home::Source::Legacy);
    assert!(!layout.marker_present);
}

#[test]
fn per_dir_envs_keep_precedence_in_the_new_layout() {
    let _env = EnvGuard::new();
    _env.set("CADENCE_HOME", "/var/lib/cadence");
    _env.set("CADENCE_PM_DIR", "/elsewhere/pm");
    _env.set("CADENCE_STATE_DIR", "/elsewhere/state");
    assert_eq!(home::tracker_dir().unwrap(), PathBuf::from("/elsewhere/pm"));
    assert_eq!(
        home::state_dir().unwrap(),
        PathBuf::from("/elsewhere/state")
    );
    // Only their own directory is overridden.
    assert_eq!(
        home::vault_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/vault")
    );
    assert_eq!(
        home::repos_dir().unwrap(),
        PathBuf::from("/var/lib/cadence/repos")
    );
}

// ---------- refusal ----------

#[test]
fn a_non_absolute_cadence_home_is_refused() {
    for bad in ["relative/dir", ".", "..", "dir/../x", ""] {
        let _env = EnvGuard::new();
        _env.set("CADENCE_HOME", bad);
        let err = home::layout().unwrap_err().to_string();
        assert!(err.contains("absolute"), "{bad:?}: {err}");
        assert!(home::home().is_err(), "{bad:?}");
        assert!(home::tracker_dir().is_err(), "{bad:?}");
        assert!(home::state_dir().is_err(), "{bad:?}");
        assert!(home::vault_dir().is_err(), "{bad:?}");
        assert!(home::repos_dir().is_err(), "{bad:?}");
        // The surfaces commands actually use refuse too.
        assert!(cadence_agent::issue::default_dir().is_err(), "{bad:?}");
        assert!(cadence_agent::client::state_dir().is_err(), "{bad:?}");
        // An explicit per-dir env still wins its own directory —
        // precedence exactly as today.
        _env.set("CADENCE_PM_DIR", "/elsewhere/pm");
        assert_eq!(
            home::tracker_dir().unwrap(),
            PathBuf::from("/elsewhere/pm"),
            "{bad:?}"
        );
        _env.remove("CADENCE_PM_DIR");
    }
}

// ---------- legacy parity ----------

/// The same env under both resolvers must give the same answer —
/// the legacy fallback is byte-identical to main before CAD-584.
#[test]
fn legacy_resolution_matches_the_old_constants() {
    let _env = EnvGuard::new();
    let (_tmp, home_dir) = home_root();
    _env.set("HOME", &home_dir);

    // Plain HOME only — today's production shape.
    assert_eq!(home::tracker_dir().unwrap(), old_issue_default_dir());
    assert_eq!(home::tracker_default().unwrap(), old_home_default_dir());
    assert_eq!(home::state_dir().unwrap(), old_state_dir());
    assert_eq!(home::state_default().unwrap(), old_default_state_dir());
    assert_eq!(home::local_state_dir().unwrap(), old_default_state_dir());
    assert_eq!(
        cadence_agent::issue::default_dir().unwrap(),
        old_issue_default_dir()
    );
    assert_eq!(
        cadence_agent::issue::home_default_dir().unwrap(),
        old_home_default_dir()
    );
    assert_eq!(cadence_agent::client::state_dir().unwrap(), old_state_dir());
    assert_eq!(
        cadence_agent::client::default_state_dir().unwrap(),
        old_default_state_dir()
    );

    // XDG and the per-dir overrides, in every combination the old
    // code distinguished.
    for (xdg, pm, state) in [
        (Some("xdg"), None, None),
        (None, Some("pmx"), None),
        (None, None, Some("sx")),
        (Some("xdg"), Some("pmx"), Some("sx")),
        (Some(""), None, None),
    ] {
        match xdg {
            Some(v) => _env.set("XDG_STATE_HOME", home_dir.join(v)),
            None => _env.remove("XDG_STATE_HOME"),
        }
        match pm {
            Some(v) => _env.set("CADENCE_PM_DIR", home_dir.join(v)),
            None => _env.remove("CADENCE_PM_DIR"),
        }
        match state {
            Some(v) => _env.set("CADENCE_STATE_DIR", home_dir.join(v)),
            None => _env.remove("CADENCE_STATE_DIR"),
        }
        assert_eq!(
            home::tracker_dir().unwrap(),
            old_issue_default_dir(),
            "xdg={xdg:?} pm={pm:?} state={state:?}"
        );
        assert_eq!(
            home::state_dir().unwrap(),
            old_state_dir(),
            "xdg={xdg:?} pm={pm:?} state={state:?}"
        );
        assert_eq!(
            cadence_agent::client::default_state_dir().unwrap(),
            old_default_state_dir(),
            "xdg={xdg:?} pm={pm:?} state={state:?}"
        );
        assert_eq!(
            cadence_agent::issue::home_default_dir().unwrap(),
            old_home_default_dir(),
            "xdg={xdg:?} pm={pm:?} state={state:?}"
        );
    }
    _env.remove("XDG_STATE_HOME");
    _env.remove("CADENCE_PM_DIR");
    _env.remove("CADENCE_STATE_DIR");

    // The legacy vault hangs off the resolved tracker — PM_DIR
    // override included.
    assert_eq!(
        home::vault_dir().unwrap(),
        old_issue_default_dir().join("wiki")
    );
    _env.set("CADENCE_PM_DIR", home_dir.join("pmx"));
    assert_eq!(home::vault_dir().unwrap(), home_dir.join("pmx/wiki"));
}

#[test]
fn legacy_home_is_the_would_be_cadence_dir() {
    let _env = EnvGuard::new();
    let (_tmp, home_dir) = home_root();
    _env.set("HOME", &home_dir);
    assert_eq!(home::home().unwrap(), home_dir.join(".cadence"));
    assert_eq!(home::repos_dir().unwrap(), home_dir.join(".cadence/repos"));
}

// ---------- small pieces ----------

#[test]
fn blobs_dir_is_dot_blobs_under_the_root() {
    assert_eq!(
        home::blobs_dir(std::path::Path::new("/any/root")),
        PathBuf::from("/any/root/.blobs")
    );
    assert_eq!(
        home::blobs_dir(std::path::Path::new("relative")),
        PathBuf::from("relative/.blobs")
    );
}

#[test]
fn local_state_dir_ignores_xdg() {
    let _env = EnvGuard::new();
    let (_tmp, home_dir) = home_root();
    _env.set("HOME", &home_dir);
    _env.set("XDG_STATE_HOME", home_dir.join("xdg"));
    assert_eq!(
        home::local_state_dir().unwrap(),
        home_dir.join(".local/state/cadence")
    );
}

// ---------- doctor --host end to end ----------

/// `cadence doctor --host` under a spawned, env-isolated binary —
/// proves the layout check reports the branch the CLI resolves.
fn doctor_layout(envs: &[(&str, &std::path::Path)]) -> serde_json::Value {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .args(["doctor", "--host", "--json"])
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(envs.iter().map(|(k, v)| (*k, v.as_os_str())))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let report: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("doctor --host --json did not emit JSON ({e}): {text}"));
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "layout")
        .cloned()
        .unwrap_or_else(|| panic!("no layout check in {report}"))
}

#[test]
fn doctor_host_prints_the_legacy_layout() {
    let _env = EnvGuard::new(); // keeps parallel tests off our envs
    let tmp = TempDir::new().unwrap();
    let home_dir = tmp.path().join("home");
    let layout = doctor_layout(&[("HOME", &home_dir)]);
    assert_eq!(layout["level"], "ok", "{layout}");
    assert_eq!(layout["value"]["source"], "legacy");
    assert_eq!(layout["value"]["marker"], false);
    assert_eq!(
        layout["value"]["tracker"],
        home_dir.join("pm").display().to_string()
    );
    assert_eq!(
        layout["value"]["state"],
        home_dir.join(".local/state/cadence").display().to_string()
    );
    assert_eq!(
        layout["value"]["vault"],
        home_dir.join("pm/wiki").display().to_string()
    );
}

#[test]
fn doctor_host_prints_the_env_layout() {
    let _env = EnvGuard::new();
    let tmp = TempDir::new().unwrap();
    let home_dir = tmp.path().join("home");
    let cad = tmp.path().join("cad");
    let layout = doctor_layout(&[("HOME", &home_dir), ("CADENCE_HOME", &cad)]);
    assert_eq!(layout["level"], "ok", "{layout}");
    assert_eq!(layout["value"]["source"], "CADENCE_HOME");
    assert_eq!(layout["value"]["marker"], false);
    assert_eq!(
        layout["value"]["tracker"],
        cad.join("tracker").display().to_string()
    );
    assert_eq!(
        layout["value"]["state"],
        cad.join("state").display().to_string()
    );
    assert_eq!(
        layout["value"]["vault"],
        cad.join("vault").display().to_string()
    );
}

/// REV-300: a planted `~/.cadence/LAYOUT` is reported but inert —
/// the check still shows the legacy branch and names the way in.
#[test]
fn doctor_host_reports_a_planted_marker_inactive() {
    let _env = EnvGuard::new();
    let tmp = TempDir::new().unwrap();
    let home_dir = tmp.path().join("home");
    std::fs::create_dir_all(home_dir.join(".cadence")).unwrap();
    std::fs::write(home_dir.join(".cadence/LAYOUT"), "1").unwrap();
    let layout = doctor_layout(&[("HOME", &home_dir)]);
    assert_eq!(layout["level"], "ok", "{layout}");
    assert_eq!(layout["value"]["source"], "legacy");
    assert_eq!(layout["value"]["marker"], true);
    let detail = layout["detail"].as_str().unwrap();
    assert!(
        detail.contains("present but inactive") && detail.contains("CADENCE_HOME"),
        "{detail}"
    );
    assert_eq!(
        layout["value"]["tracker"],
        home_dir.join("pm").display().to_string()
    );
    assert_eq!(
        layout["value"]["state"],
        home_dir.join(".local/state/cadence").display().to_string()
    );
    assert_eq!(
        layout["value"]["vault"],
        home_dir.join("pm/wiki").display().to_string()
    );
}
