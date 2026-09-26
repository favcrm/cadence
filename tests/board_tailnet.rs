//! board_tailnet: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::json;
use serde_json::Value;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

/// A fake `tailscale` first on PATH: `status` answers from
/// `status.json` (+ optional `status.rc`/`status.err`), `serve` keeps a
/// `key→target` map in `serve.map` and reports it as
/// `serve status --json`. Every argv lands in `calls.log`. The map is
/// keyed `<dns>:<port>` like the real `Web` object.
fn fake_ts() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let d = &dir.path().to_path_buf();
    std::fs::write(d.join("dns"), TS_DNS).unwrap();
    std::fs::write(d.join("calls.log"), "").unwrap();
    std::fs::write(
        d.join("status.json"),
        format!(
            r#"{{"BackendState":"Running","Self":{{"DNSName":"{TS_DNS}."}},"CertDomains":["{TS_DNS}"]}}"#
        ),
    )
    .unwrap();
    let script = r#"#!/usr/bin/env bash
d="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo "$*" >> "$d/calls.log"
case "${1:-}" in
status)
  [ -f "$d/status.err" ] && cat "$d/status.err" >&2
  [ -f "$d/status.json" ] && cat "$d/status.json"
  exit "$(cat "$d/status.rc" 2>/dev/null || echo 0)"
  ;;
serve)
  case "${2:-}" in
  status)
    printf '{"Web":{'
    first=1
    if [ -f "$d/serve.map" ]; then
      while IFS=$'\t' read -r key target; do
        [ -n "$key" ] || continue
        [ "$first" -eq 0 ] && printf ','
        first=0
        printf '"%s":{"Handlers":{"/":{"Proxy":"%s"}}}' "$key" "$target"
      done < "$d/serve.map"
    fi
    printf '}}'
    ;;
  --bg)
    port="${3#--https=}"
    printf '%s:%s\t%s\n' "$(cat "$d/dns")" "$port" "$4" >> "$d/serve.map"
    ;;
  --https=*)
    if [ "${3:-}" = "off" ]; then
      key="$(cat "$d/dns"):${2#--https=}"
      grep -v "^$key" "$d/serve.map" > "$d/.sm" 2>/dev/null || true
      mv "$d/.sm" "$d/serve.map"
    fi
    ;;
  esac
  ;;
esac
exit 0
"#;
    let path = d.join("tailscale");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (dir, d.to_path_buf())
}

/// PATH with the fake `tailscale` dir first, then the built binary,
/// then ambient — `cli_env`'s env overrides its own PATH default.
fn ts_env(fake: &Path) -> [(&'static str, String); 1] {
    [(
        "PATH",
        format!(
            "{}:{}:{}",
            fake.display(),
            Path::new(bin()).parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )]
}

/// Write a `ui.json` carrying just the port — the "board not running"
/// path of `ui tailscale start` picks its target from it.
fn seed_ui_port(state: &Path, port: u16) {
    std::fs::write(state.join("ui.json"), format!(r#"{{"port":{port}}}"#)).unwrap();
}

fn ui_opts(state: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state.join("ui.json")).unwrap()).unwrap()
}

#[test]
fn ui_tailscale_start_shares_and_persists() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let port = free_port();
    seed_ui_port(state.path(), port);
    let _ui = DetachedUi(state.path().to_path_buf());

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "start", "--port", "9450"],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["state"], "sharing");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    assert_eq!(out["board"], "started");
    assert_eq!(out["mapping_created"], true);

    // The mapping was created exactly once; funnel never invoked.
    let c = calls(&fake);
    assert_eq!(c.matches("serve --bg --https=9450").count(), 1, "{c}");
    assert!(!c.contains("funnel"), "{c}");

    // Options persisted: tailnet block + derived names resolvable.
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["dns_name"], TS_DNS);
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["tailscale"]["target"], format!("http://127.0.0.1:{port}"));

    // `ui status` reports the tailnet URL and the running board.
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "status"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["state"], "running");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));

    // `ui tailscale status` prints the URL, the live mapping, and the
    // identity probe: a local process posing as the proxy is ignored
    // (CAD-336) — the probe is exactly the forgery it must refuse.
    let (ok, text) = cli_raw_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "status"],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains(&format!("https://{TS_DNS}:9450")), "{text}");
    assert!(text.contains("(live)"), "{text}");
    assert!(
        text.contains("identity: local forged login ignored (operator (ui); refused by check"),
        "{text}"
    );
    assert!(!text.contains("FORGEABLE"), "{text}");

    // Second start is idempotent: no new mapping, board restarted.
    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "start", "--port", "9450"],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["mapping_created"], false);
    assert_eq!(out["board"], "restarted");
    assert_eq!(
        calls(&fake).matches("serve --bg").count(),
        1,
        "{}",
        calls(&fake)
    );
}

#[test]
fn ui_tailscale_start_while_board_runs_restarts() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());

    // Plain `ui start --port` first — the board is already running
    // local-only when the operator shares it.
    let port = free_port();
    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "start", "--port", &port.to_string()],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["state"], "started");

    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["board"], "restarted");
    // Default https port is 9450; the target uses the persisted port.
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["tailscale"]["target"], format!("http://127.0.0.1:{port}"));
}

#[test]
fn ui_tailscale_conflicting_mapping_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    // A foreign mapping already owns :9450.
    std::fs::write(
        fake.join("serve.map"),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n"),
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("refusing to overwrite"), "{msg}");
    assert!(msg.contains("http://127.0.0.1:9999"), "{msg}");
    // The foreign mapping is untouched; no ui.json was written.
    assert_eq!(
        std::fs::read_to_string(fake.join("serve.map")).unwrap(),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n")
    );
    assert!(!state.path().join("ui.json").exists());
    // And no board got started.
    let (_, out) = cli_env(pm.path(), state.path(), &["ui", "status"], &env);
    assert_eq!(out["state"], "stopped");
}

#[test]
fn ui_tailscale_logged_out_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(fake.join("status.rc"), "1").unwrap();
    std::fs::write(fake.join("status.err"), "Logged out.\n").unwrap();
    std::fs::remove_file(fake.join("status.json")).unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("logged out"),
        "{err}"
    );
    assert!(!state.path().join("ui.json").exists());
}

#[test]
fn ui_tailscale_no_https_certs_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(
        fake.join("status.json"),
        format!(
            r#"{{"BackendState":"Running","Self":{{"DNSName":"{TS_DNS}."}},"CertDomains":[]}}"#
        ),
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("HTTPS certificates"),
        "{err}"
    );
}

#[test]
fn ui_tailscale_not_running_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(
        fake.join("status.json"),
        r#"{"BackendState":"Stopped","Self":{"DNSName":"node.tail1234.ts.net."},"CertDomains":["node.tail1234.ts.net"]}"#,
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(err["error"].as_str().unwrap().contains("not up"), "{err}");
}

#[test]
fn ui_start_tailscale_requires_loopback() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);

    // The refusal must precede any tailscale subprocess call.
    let (ok, err) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "start", "--host", "0.0.0.0", "--tailscale"],
        &env,
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("not loopback"),
        "{err}"
    );
    assert!(
        calls(&fake).is_empty(),
        "tailscale never ran: {}",
        calls(&fake)
    );
}

#[test]
fn ui_start_tailscale_flag_alias() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &[
            "ui",
            "start",
            "--port",
            &port.to_string(),
            "--tailscale",
            "--read-only",
        ],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["read_only"], true);
}

#[test]
fn ui_tailscale_survives_stop_start_cycle() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();
    seed_ui_port(state.path(), port);

    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);

    // `ui stop` leaves the mapping; `ui start` re-ensures (no new
    // --bg) and the board comes back shared.
    assert!(cli_env(pm.path(), state.path(), &["ui", "stop"], &env).0);
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "start"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    assert_eq!(
        calls(&fake).matches("serve --bg").count(),
        1,
        "{}",
        calls(&fake)
    );
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
}

#[test]
fn ui_stop_tailscale_off_removes_only_cadences_mapping() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();
    seed_ui_port(state.path(), port);
    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);
    // A foreign mapping on another port must survive --tailscale-off.
    std::fs::write(
        fake.join("serve.map"),
        format!(
            "{TS_DNS}:9450\thttp://127.0.0.1:{port}\nother.host.ts.net:8443\thttp://127.0.0.1:9999\n"
        ),
    )
    .unwrap();

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "stop", "--tailscale-off"],
        &env,
    );
    assert!(ok, "{out}");
    let map = std::fs::read_to_string(fake.join("serve.map")).unwrap();
    assert!(!map.contains(":9450"), "{map}");
    assert!(map.contains("other.host.ts.net:8443"), "{map}");
    assert_eq!(ui_opts(state.path())["tailscale"], Value::Null);
    assert!(
        calls(&fake).contains("serve --https=9450 off"),
        "{}",
        calls(&fake)
    );
}

#[test]
fn ui_tailscale_stop_foreign_mapping_left_alone() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    seed_ui_port(state.path(), free_port());
    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);
    // Someone else claimed :9450 after us — stop must not remove it.
    std::fs::write(
        fake.join("serve.map"),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n"),
    )
    .unwrap();

    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "stop"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["mapping_removed"], false);
    let map = std::fs::read_to_string(fake.join("serve.map")).unwrap();
    assert!(map.contains("http://127.0.0.1:9999"), "{map}");
    assert!(!calls(&fake).contains("off"), "{}", calls(&fake));
    assert_eq!(ui_opts(state.path())["tailscale"], Value::Null);
}

#[test]
fn ui_tailscale_stop_when_not_sharing() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "stop"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["state"], "not_sharing");
}

/// This test process's user name — the board's user in board tests.
fn own_user_name() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `/api/meta` for a tailnet-shaped request carrying a forged login:
/// `(actor, tailnet_proof)`.
fn tailnet_meta(port: u16) -> (String, Value) {
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta",
        &format!("{TS_DNS}:9450"),
        &["Tailscale-User-Login: mallory@evil.example"],
        b"",
    );
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    (
        meta["actor"].as_str().unwrap_or_default().to_string(),
        meta["tailnet_proof"].clone(),
    )
}

/// CAD-336: a local process sending exactly the proxy's shape —
/// tailnet Host, https Origin, identity headers, loopback peer — against
/// a tailscaled that passes every config check (kernel networking, no
/// TCP forwarder to the board) is still not the proxy: here the fake
/// tailscaled is this test's own uid, so the last check, `foreign_uid`,
/// refuses (on a real host the local caller fails `socket_owner`, which
/// `tailnet_proof`'s unit tests cover — a test cannot open a socket as
/// another uid). It writes as itself, `operator (ui)`, and the forged
/// login is never recorded.
#[test]
fn tailnet_shaped_local_write_is_not_the_proxy() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let ts_host = format!("{TS_DNS}:9450");
    let origin = format!("https://{TS_DNS}:9450");

    // CAD-313/CAD-428: an unproven tailnet request can use no session —
    // not even the operator's loopback one replayed onto it — so the
    // local caller is refused and nothing is written, under no name.
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let before = commits(pm.path());
    let mut headers = ts_write_headers(&origin, "fable@example.com");
    for cookie in [None, Some(format!("Cookie: {}", op.cookie))] {
        headers.retain(|h| !h.starts_with("Cookie:") && !h.starts_with("X-Cadence-Session:"));
        if cookie.is_some() {
            headers.push(op.key_header());
        }
        headers.extend(cookie);
        let href: Vec<&str> = headers.iter().map(String::as_str).collect();
        let (code, _, body) = http_write(
            port,
            "PATCH",
            "/api/issues/CAD-2",
            &ts_host,
            &href,
            br#"{"status":"done"}"#,
        );
        assert_eq!(code, 403, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["check"], "operator_session_required", "{v}");
        assert!(!body.contains("fable"), "{body}");
    }
    assert_eq!(commits(pm.path()), before, "nothing is written");

    // /api/meta agrees, names the refusing check, and still reports
    // the tailnet URL.
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["proven"], false, "{proof}");
    assert_eq!(proof["check"], "foreign_uid", "{proof}");
    let (_, _, body) = http_write(port, "GET", "/api/meta", &ts_host, &[], b"");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["tailnet_url"],
        origin
    );
}

/// CAD-336: each fail-closed check of the tailnet proof refuses on its
/// own condition and `/api/meta` names it — so a refusal can never pass
/// for a different reason (on a host without tailscaled every request
/// would otherwise fail at `tailscaled_socket`). One board per case: the
/// LocalAPI facts are cached per socket.
#[test]
fn tailnet_proof_refusals_name_their_check() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    type Setup = fn(&Path, u16);
    let cases: [(&str, Setup); 9] = [
        ("localapi", |d, p| {
            // No TUN field: the status cannot be read as either mode.
            localapi_says(d, None, serve_https_only(p))
        }),
        ("localapi", |d, _| {
            // The serve config does not answer.
            localapi_says(d, Some(true), json!({}));
            std::fs::remove_file(d.join("serve.json")).unwrap();
        }),
        ("localapi", |d, p| {
            // The prefs do not answer: the operator user is unknown.
            localapi_says(d, Some(true), serve_https_only(p));
            std::fs::remove_file(d.join("prefs.json")).unwrap();
        }),
        ("kernel_networking", |d, p| {
            localapi_says(d, Some(false), serve_https_only(p))
        }),
        ("not_operator_user", |d, p| {
            // The board's user is tailscaled's operator: it could make
            // tailscaled dial the board at will (qa-1 round 2).
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, &own_user_name());
        }),
        ("not_operator_user", |d, p| {
            // An operator name that resolves to no user: fail closed.
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, "no-such-user-cad336");
        }),
        ("foreign_uid", |d, p| {
            // CAD-509: tailscaled omits an empty OperatorUser
            // (omitempty) — a prefs object without the field is "no
            // operator", so the rungs pass through to the fixture's
            // own-uid refusal.
            localapi_says(d, Some(true), serve_https_only(p));
            std::fs::write(d.join("prefs.json"), r#"{"WantRunning": true}"#).unwrap();
        }),
        ("no_tcp_forwarder", |d, p| {
            // qa-1's attack: `tailscale serve --tcp=N tcp://127.0.0.1:<board>`
            // in a foreground session — raw TCP passes forged headers.
            let mut serve = serve_https_only(p);
            serve["Foreground"] = json!({"sess1": {"TCP": {"7777": {
                "TCPForward": format!("127.0.0.1:{p}")
            }}}});
            localapi_says(d, Some(true), serve)
        }),
        ("foreign_uid", |d, p| {
            // Another user (root) is the operator: every config check
            // passes, and the fake tailscaled's own uid refuses last.
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, "root");
        }),
    ];
    for (want, setup) in cases {
        let (ts_dir, sock) = fake_localapi();
        let (port, _board) = start_ui_opts(
            pm.path().to_path_buf(),
            state.path().to_path_buf(),
            tailnet_opts(&sock),
        );
        setup(ts_dir.path(), port);
        let (actor, proof) = tailnet_meta(port);
        assert_eq!(actor, "operator (ui)", "{want}");
        assert_eq!(proof["proven"], false, "{want}: {proof}");
        assert_eq!(proof["check"], want, "{proof}");
    }

    // No LocalAPI socket at all: tailscaled's uid is unknown.
    let nowhere = TempDir::new().unwrap().path().join("absent.sock");
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&nowhere),
    );
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "tailscaled_socket", "{proof}");
}

/// CAD-336 r4 (qa-1 round 3): the operator check latches for the
/// board process's life. A board whose user was tailscaled's operator
/// at startup stays refused after the operator is cleared — a
/// connection set up through a since-removed forwarder would outlive
/// the clear — and so does a board whose startup read failed.
#[test]
fn tailnet_operator_latch_outlives_a_clear() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());

    // The board's user is the operator at startup, then clears itself.
    let (ts_dir, sock) = fake_localapi();
    localapi_operator(ts_dir.path(), &own_user_name());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "operator_latched", "{proof}");

    // The startup read fails (no prefs), then the LocalAPI recovers.
    let (ts_dir, sock) = fake_localapi();
    std::fs::remove_file(ts_dir.path().join("prefs.json")).unwrap();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "operator_latched", "{proof}");

    // Sighted as operator AFTER startup: latched from then on too.
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    localapi_operator(ts_dir.path(), &own_user_name());
    assert_eq!(tailnet_meta(port).1["check"], "not_operator_user");
    localapi_operator(ts_dir.path(), "");
    // Past the LocalAPI cache: the fresh read shows no operator.
    thread::sleep(Duration::from_millis(2100));
    assert_eq!(tailnet_meta(port).1["check"], "operator_latched");
}

/// The TCP-forwarder attack end to end: a tailnet-shaped write through
/// a board whose serve config forwards raw TCP to it is not the proxy,
/// so it names nobody — and, holding no session, writes nothing.
#[test]
fn tailnet_write_with_a_tcp_forwarder_is_not_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    let mut serve = serve_https_only(port);
    serve["TCP"]["7777"] = json!({"TCPForward": format!("127.0.0.1:{port}")});
    localapi_says(ts_dir.path(), Some(true), serve);
    let origin = format!("https://{TS_DNS}:9450");
    let before = commits(pm.path());
    let headers = ts_write_headers(&origin, "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &format!("{TS_DNS}:9450"),
        &href,
        br#"{"status":"done"}"#,
    );
    // CAD-313: unproven, so no session and no operator — refused.
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("operator_session_required"), "{body}");
    assert!(!body.contains("mallory"), "{body}");
    assert_eq!(commits(pm.path()), before, "nothing is written");
    assert_eq!(tailnet_meta(port).1["check"], "no_tcp_forwarder");
}

#[test]
fn forged_tailscale_headers_not_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(Path::new("/nonexistent/tailscaled.sock")),
    );
    let host = format!("127.0.0.1:{port}");

    // Same headers, but the Host is direct loopback — the identity
    // headers must be ignored (the proxy only sets them on the
    // tailnet name). Without a session that is a refusal (CAD-313);
    // with the operator's, `operator (ui)` — never the forged login.
    let headers = ts_write_headers(&format!("http://127.0.0.1:{port}"), "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &href,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("operator_session_required"), "{body}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    // On the board's own Host (where the session lives) with its Origin.
    let signed: Vec<&str> = href
        .iter()
        .copied()
        .filter(|h| !h.starts_with("Origin:"))
        .collect();
    let (code, _, _) = op_http_write(
        &op,
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &signed,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 200);
    let sha = sha_of(pm.path(), "cadence/CAD-2", "status=done");
    let t = trailers_of(pm.path(), &sha);
    assert!(t.contains("Actor: operator (ui)"), "{t}");
    assert!(!t.contains("mallory"), "{t}");

    // And meta agrees: the forged header never resolves.
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta",
        &host,
        &["Tailscale-User-Login: mallory@evil.example"],
        b"",
    );
    assert_eq!(code, 200);
    let meta = serde_json::from_str::<Value>(&body).unwrap();
    assert_eq!(meta["actor"], "operator (ui)");
    // Not tailnet-shaped at all: no proof was even attempted.
    assert_eq!(meta["tailnet_proof"], Value::Null);
}

#[test]
fn tailnet_write_wrong_origin_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(Path::new("/nonexistent/tailscaled.sock")),
    );
    // Tailnet Host but a foreign Origin — still refused.
    let headers = ts_write_headers("https://evil.example", "fable@example.com");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &format!("{TS_DNS}:9450"),
        &href,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "origin"
    );
}
