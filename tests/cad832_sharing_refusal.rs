//! CAD-832 independent refusal acceptance for persisted tailnet targets.
//!
//! The real `cadence ui start` CLI must reject a forged persisted proxy
//! target before calling `tailscale serve --bg`, leaving `ui.json` byte for
//! byte intact. An honest target with the same isolated sandbox profile must
//! reach that real command path and be accepted. The private `tailscale`
//! executable only records and simulates external command effects; it does
//! not implement or decide Cadence authorization.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::{Builder, TempDir};

const BINARY: &str = env!("CARGO_BIN_EXE_cadence");
const PORT: u16 = 3117;
const SHARE_PORT: u16 = 19450;
const TARGET: &str = "http://127.0.0.1:3117";
const FORGED_TARGET: &str = "http://127.0.0.1:3010";

struct Attempt {
    original: Vec<u8>,
    persisted: Vec<u8>,
    output: Output,
    tailscale_calls: String,
}

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    state: PathBuf,
    fake_bin: PathBuf,
    log: PathBuf,
    map: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let root = Builder::new()
            .prefix(&format!("c832-{label}-"))
            .tempdir_in("/tmp")
            .expect("short private fixture root");
        let home = root.path().join("h");
        let state = root.path().join("s");
        let fake_bin = root.path().join("bin");
        let log = root.path().join("tailscale.log");
        let map = root.path().join("serve-map.json");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&fake_bin).unwrap();
        fs::write(fake_bin.join("tailscale"), FAKE_TAILSCALE).unwrap();
        fs::set_permissions(
            fake_bin.join("tailscale"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        Self {
            _root: root,
            home,
            state,
            fake_bin,
            log,
            map,
        }
    }

    fn write_opts(&self, target: &str) -> Vec<u8> {
        let bytes = format!(
            "{{\n  \"port\": {PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{target}\"\n  }}\n}}\n"
        )
        .into_bytes();
        fs::write(self.state.join("ui.json"), &bytes).unwrap();
        bytes
    }

    fn write_unshared_opts(&self) -> Vec<u8> {
        let bytes = format!("{{\n  \"port\": {PORT}\n}}\n").into_bytes();
        fs::write(self.state.join("ui.json"), &bytes).unwrap();
        bytes
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(BINARY);
        cmd.args(["--state-dir"])
            .arg(&self.state)
            .args(args)
            .current_dir(self._root.path())
            .env("HOME", &self.home)
            .env("XDG_STATE_HOME", self._root.path().join("xdg-state"))
            .env("XDG_CONFIG_HOME", self._root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self._root.path().join("xdg-data"))
            .env("XDG_CACHE_HOME", self._root.path().join("xdg-cache"))
            .env("CADENCE_PROFILE", "sandbox:cad832-acceptance")
            .env("CADENCE_SANDBOX_ALLOW_GLOBAL", "1")
            .env("CADENCE_PM_DIR", self._root.path().join("pm"))
            .env("CADENCE_SANDBOX_ROOT", self._root.path().join("sandboxes"))
            .env("TS_LOG", &self.log)
            .env("TS_MAP", &self.map)
            .env("PATH", prefixed_path(&self.fake_bin))
            .env_remove("CADENCE_ALIAS")
            .env_remove("CADENCE_ROLLOUT_AS")
            .env_remove("CADENCE_STATE_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        cadence_agent::reaper::output(&mut cmd).expect("invoke real cadence CLI")
    }

    fn tailscale_calls(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best-effort cleanup for the private UI process and fake mapping if
        // an assertion panics after the honest control starts.
        let _ = self.cli(&["ui", "stop"]);
        let _ = self.cli(&["ui", "tailscale", "stop"]);
    }
}

fn prefixed_path(bin: &Path) -> String {
    format!(
        "{}:{}",
        bin.display(),
        std::env::var_os("PATH")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    )
}

fn attempt(label: &str, args: &[&str], target: &str) -> Attempt {
    let fixture = Fixture::new(label);
    let original = fixture.write_opts(target);
    capture_attempt(fixture, args, original)
}

fn attempt_unshared(label: &str, args: &[&str]) -> Attempt {
    let fixture = Fixture::new(label);
    let original = fixture.write_unshared_opts();
    capture_attempt(fixture, args, original)
}

fn capture_attempt(fixture: Fixture, args: &[&str], original: Vec<u8>) -> Attempt {
    let output = fixture.cli(args);
    let persisted = fs::read(fixture.state.join("ui.json")).unwrap();
    let tailscale_calls = fixture.tailscale_calls();
    Attempt {
        original,
        persisted,
        output,
        tailscale_calls,
    }
}

fn has_mapping_write(attempt: &Attempt) -> bool {
    attempt
        .tailscale_calls
        .lines()
        .any(|line| line.starts_with("serve --bg") || line.ends_with(" off"))
}

fn output_text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn persisted_production_target_refuses_before_mapping_and_honest_target_reaches_cli() {
    // The refusal is on the real persisted-ui.json -> `ui start` route,
    // with the sandbox's explicit sharing grant present so that the target
    // validation, not the opt-in gate, determines the result.
    let forged = Fixture::new("forged");
    let original = forged.write_opts(FORGED_TARGET);
    let rejected = forged.cli(&["ui", "start"]);
    assert!(
        !rejected.status.success(),
        "forged production target unexpectedly accepted: {}",
        output_text(&rejected)
    );
    assert_eq!(
        fs::read(forged.state.join("ui.json")).unwrap(),
        original,
        "refusal must preserve the persisted share record"
    );
    assert!(
        forged.tailscale_calls().is_empty(),
        "refusal must precede every tailscale side effect: {}",
        forged.tailscale_calls()
    );
    assert!(
        !forged.tailscale_calls().contains("serve --bg"),
        "forged target reached tailscale serve --bg"
    );

    // Both public mutation routes must validate the pre-existing record,
    // even when the command also supplies a replacement share setting.
    // Keep each route's state, recorder, and any spawned board isolated.
    let start_with_flag = attempt(
        "start-flag-forged",
        &["ui", "start", "--tailscale", "19450"],
        FORGED_TARGET,
    );
    let tailscale_start = attempt(
        "tailscale-start-forged",
        &["ui", "tailscale", "start", "--port", "19450"],
        FORGED_TARGET,
    );
    // A partial device-login tuple must be rejected before the explicit
    // --tailscale option writes a mapping. This fixture has a loopback
    // sandbox port but no persisted share record to mask the ordering bug.
    let partial_device_login = attempt_unshared(
        "partial-device-login",
        &[
            "ui",
            "start",
            "--tailscale",
            "19450",
            "--device-login-issuer",
            "https://issuer.example",
        ],
    );
    // Honest controls prove both commands reach their real mapping path.
    let start_with_flag_honest = attempt(
        "start-flag-honest",
        &["ui", "start", "--tailscale", "19450"],
        TARGET,
    );
    let tailscale_start_honest = attempt(
        "tailscale-start-honest",
        &["ui", "tailscale", "start", "--port", "19450"],
        TARGET,
    );
    let mismatch_refusal = |a: &Attempt| {
        let text = output_text(&a.output).to_ascii_lowercase();
        !a.output.status.success()
            && text.contains("persisted tailnet target")
            && text.contains("effective bind")
            && a.persisted == a.original
            && !has_mapping_write(a)
    };
    let honest_route_reached = |a: &Attempt| {
        a.output.status.success()
            && a.tailscale_calls
                .lines()
                .any(|line| line == "serve --bg --https=19450 http://127.0.0.1:3117")
    };
    let partial_device_refusal = {
        let text = output_text(&partial_device_login.output).to_ascii_lowercase();
        !partial_device_login.output.status.success()
            && text.contains("device login needs issuer, org and at least one subject")
            && partial_device_login.persisted == partial_device_login.original
            && !has_mapping_write(&partial_device_login)
    };
    assert!(
        mismatch_refusal(&start_with_flag)
            && mismatch_refusal(&tailscale_start)
            && partial_device_refusal,
        "mutation routes must refuse before mapping writes; ui start --tailscale forged: \
         status={:?}, output={}, unchanged={}, calls={:?}; ui tailscale start forged: \
         status={:?}, output={}, unchanged={}, calls={:?}; partial device-login: \
         status={:?}, output={}, unchanged={}, calls={:?}",
        start_with_flag.output.status,
        output_text(&start_with_flag.output),
        start_with_flag.persisted == start_with_flag.original,
        start_with_flag.tailscale_calls,
        tailscale_start.output.status,
        output_text(&tailscale_start.output),
        tailscale_start.persisted == tailscale_start.original,
        tailscale_start.tailscale_calls,
        partial_device_login.output.status,
        output_text(&partial_device_login.output),
        partial_device_login.persisted == partial_device_login.original,
        partial_device_login.tailscale_calls,
    );
    assert!(
        honest_route_reached(&start_with_flag_honest)
            && honest_route_reached(&tailscale_start_honest),
        "honest targets must reach each real CLI mapping route; ui start --tailscale: {}; \
         ui tailscale start: {}",
        output_text(&start_with_flag_honest.output),
        output_text(&tailscale_start_honest.output)
    );

    // Honest control uses the ordinary `ui start` CLI command and sandbox
    // profile. It must start the loopback board and reach the recorder too.
    let honest = Fixture::new("honest");
    honest.write_opts(TARGET);
    let accepted = honest.cli(&["ui", "start"]);
    assert!(
        accepted.status.success(),
        "honest persisted target did not reach ui start: {}",
        output_text(&accepted)
    );
    let calls = honest.tailscale_calls();
    assert!(
        calls
            .lines()
            .any(|line| line == "serve --bg --https=19450 http://127.0.0.1:3117"),
        "honest route did not invoke the recorded serve mapping: {calls}"
    );
    let persisted = fs::read_to_string(honest.state.join("ui.json")).unwrap();
    assert!(
        persisted.contains(TARGET),
        "honest control did not retain the accepted target: {persisted}"
    );

    // Reset must ask the real `ui tailscale stop` path to prove removal.
    // The recorder reports a different, foreign live target for this port;
    // the reset must fail closed without deleting either durable record.
    let reset = Fixture::new("reset");
    let base = reset._root.path().join("sandboxes");
    let root = base.join("refusal");
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        root.join(".cadence-sandbox"),
        r#"{"name":"refusal","allow_global":true}"#,
    )
    .unwrap();
    let record = format!(
        "{{\n  \"port\": {PORT},\n  \"tailscale\": {{\n    \"dns_name\": \"acceptance.example.ts.net\",\n    \"https_port\": {SHARE_PORT},\n    \"target\": \"{TARGET}\"\n  }}\n}}\n"
    );
    fs::write(state.join("ui.json"), &record).unwrap();
    fs::write(
        &reset.map,
        format!(
            "{{\"Web\":{{\"acceptance.example.ts.net:{SHARE_PORT}\":{{\"Handlers\":{{\"/\":{{\"Proxy\":\"http://127.0.0.1:3118\"}}}}}}}}}}\n"
        ),
    )
    .unwrap();
    let refused = reset.cli(&["sandbox", "reset", "refusal"]);
    assert!(
        !refused.status.success(),
        "reset unexpectedly removed a foreign mapping: {}",
        output_text(&refused)
    );
    assert!(
        root.is_dir(),
        "failed removal must preserve the sandbox root"
    );
    assert_eq!(
        fs::read_to_string(state.join("ui.json")).unwrap(),
        record,
        "failed removal must preserve the mapping record"
    );
    assert!(
        !reset
            .tailscale_calls()
            .lines()
            .any(|line| line.ends_with(" off")),
        "foreign mapping must never be removed: {}",
        reset.tailscale_calls()
    );
}

const FAKE_TAILSCALE: &str = r##"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$TS_LOG"
if [ "$1" = "status" ] && [ "$2" = "--json" ]; then
  printf '%s\n' '{"BackendState":"Running","Self":{"DNSName":"acceptance.example.ts.net."},"CertDomains":["acceptance.example.ts.net"]}'
  exit 0
fi
if [ "$1" = "serve" ] && [ "$2" = "status" ] && [ "$3" = "--json" ]; then
  if [ -f "$TS_MAP" ]; then cat "$TS_MAP"; else printf '%s\n' '{"Web":{}}'; fi
  exit 0
fi
if [ "$1" = "serve" ] && [ "$2" = "--bg" ]; then
  port=${3#--https=}
  target=$4
  printf '{"Web":{"acceptance.example.ts.net:%s":{"Handlers":{"/":{"Proxy":"%s"}}}}}\n' "$port" "$target" > "$TS_MAP"
  exit 0
fi
if [ "$1" = "serve" ] && [ "$3" = "off" ]; then
  rm -f "$TS_MAP"
  exit 0
fi
printf 'unexpected fake tailscale invocation: %s\n' "$*" >&2
exit 2
"##;
