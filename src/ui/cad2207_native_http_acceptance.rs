//! CAD-2207 native HTTP acceptance — independent author.
//!
//! The P1 regression: a member's private personal-assistant PROCESS presents
//! a signed-in human's board session cookie. It must not be classified as
//! that human and must not use the human's member/operator HTTP authority.
//! Implementers must not edit or weaken these bytes.
//!
//! Unlike the daemon-level checks this drives the real HTTP boundary end to
//! end: the parent fixture's real `crate::ui::serve` board on a loopback
//! port, the real `crate::daemon::serve_with` daemon on a Unix socket, the
//! real platform sign-in / JWKS / board-authorize / revalidate fixture, a
//! REAL signed SSO exchange that mints the human's `__Host-aos-board-session`
//! cookie, and a REAL separate OS process (a spawned shell) that opens its
//! own TCP connection and sends the member POST. Attribution is the
//! production `/proc` one: the process is recorded exactly as a live pane
//! endpoint is (alias, pid, the pid's `/proc` start time, endpoint kind
//! `pty`, a generation). No `X-Cadence-Test-As` assertion header and no
//! caller-chosen environment alias is used as identity evidence anywhere in
//! this check.
//!
//! The bypass this pins: the daemon's `agent_list` now withholds a member's
//! private assistant row from every caller but that assistant itself. A
//! board that built its pid→alias attribution roots from that filtered list
//! would stop recognising the assistant's process, read the human cookie as
//! the human's own, and relay member authority to the assistant. The board
//! therefore attributes from the narrow operator-only `agent_operational`
//! projection, which still carries pid→alias and no profile. Both halves are
//! asserted against the REAL daemon: the private row is absent from
//! `agent_list` and present in `agent_operational` (the bypass is live), and
//! the bad HTTP request is then refused at `board_caller` with the presented
//! session revoked — the only effect of the board naming the caller as that
//! agent.
//!
//! The positive control runs first on the same fixture, route, cookie and
//! fixture authority: the human's own request (from a process tied to no
//! agent) is admitted and reaches the member surface, so a blanket refusal
//! cannot pass these checks.
//!
//! Deliberately NOT exercised here (reported, never simulated): the
//! `daemon restart --when-idle` busy snapshot and the owner-authorized
//! resume are CLI-level paths (`src/cli/daemon.rs`) with their own
//! acceptance; this HTTP file neither restarts nor resumes the fixture.

#![cfg(all(test, feature = "test-seam"))]

use super::super::{start, Rig};
use crate::daemon::team_assistants_refusal::AssistantFlagOn;
use crate::daemon::ServeOptions;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The alias the member's stored binding names (the fixture's
/// `start(&["alice"])` binds exactly this), asserted by the positive control
/// before any attribution decision below.
const PRIVATE_ALIAS: &str = "assist-agent-alice";
/// The endpoint generation a live pane row records. `agent_roots` treats a
/// `pty` row without one as no root at all, so a fixture that omitted it
/// would attribute nothing.
const PANE_GENERATION: &str = "cad2207-fixture-generation";
/// A nonmember alias that must stay observable in the same fixture, so an
/// empty list or an over-broad filter cannot pass as the private-row filter.
const ORDINARY_ALIAS: &str = "cad2207-ordinary-worker";

/// The wire the spawned process sends: a browser-shaped member POST carrying
/// the human's cookie. `\r\n` is printf's; the shell is the request client, so
/// the request is not made by the test's own process.
const REQUEST_SCRIPT: &str = r#"exec 3<>/dev/tcp/127.0.0.1/$PORT || exit 91
printf 'POST /api/member-assistant/state HTTP/1.1\r\nHost: %s\r\nOrigin: http://%s\r\nSec-Fetch-Site: same-origin\r\nX-Cadence-Board: 1\r\nContent-Type: application/json\r\nCookie: __Host-aos-board-session=%s\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}' "$HOST" "$HOST" "$COOKIE" >&3
cat <&3
"#;

/// A real, separate process that will make the request as the member's
/// private assistant. It inherits no Cadence identity: `env_clear` removes
/// `CADENCE_ALIAS`, `CADENCE_TEST_AS` and the seam arming variable, so its
/// only claim to a caller is the pane row the daemon records below.
fn spawn_private_assistant(rig: &Rig, token: &str) -> Child {
    Command::new("/bin/bash")
        .arg("-s")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOST", &rig.public_host)
        .env("PORT", rig.board_port.to_string())
        .env("COOKIE", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the fixture cannot spawn a private member-assistant process")
}

/// Hand the request to the already-spawned process (so its pid was recorded
/// as the assistant before it connects), read its raw HTTP answer.
fn drive(mut child: Child) -> (u16, Value) {
    {
        let mut stdin = child
            .stdin
            .take()
            .expect("the private process lost its script input");
        stdin
            .write_all(REQUEST_SCRIPT.as_bytes())
            .expect("the fixture cannot hand the private process its request");
    }
    let output = child
        .wait_with_output()
        .expect("the private process did not exit");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (head, body) = stdout.split_once("\r\n\r\n").unwrap_or_else(|| {
        panic!(
            "the private process answered no HTTP response (stderr: {})",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("the private process answered no HTTP status: {head}"));
    (status, serde_json::from_str(body).unwrap_or(Value::Null))
}

fn aliases(list: &Value) -> Vec<String> {
    list["agents"]
        .as_array()
        .unwrap_or_else(|| panic!("the daemon's agent list is not a list: {list}"))
        .iter()
        .filter_map(|agent| agent["alias"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn private_member_assistant_process_cannot_ride_a_humans_board_session() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let state = rig._dir.path().to_path_buf();

    // ---- The human session: the REAL signed SSO exchange ------------------
    // The board verifies the fixture assertion, the daemon consumes its jti
    // and mints the `__Host-aos-board-session` cookie; nothing is inserted
    // into the session store by hand.
    let opened = rig.member_post(
        "/__platform/session",
        None,
        &json!({"assertion": rig.alice_sso()}),
    );
    assert_eq!(
        opened.status().as_u16(),
        200,
        "the fixture's real signed SSO exchange was refused"
    );
    let cookie = opened
        .headers()
        .get("set-cookie")
        .expect("the signed SSO minted no public cookie")
        .to_str()
        .expect("the cookie is not ASCII")
        .split(';')
        .next()
        .expect("the cookie is empty")
        .to_string();
    drop(opened);
    assert!(
        cookie.starts_with("__Host-aos-board-session="),
        "the SSO minted an unexpected cookie name"
    );
    let token = cookie
        .split_once('=')
        .expect("the cookie has no value")
        .1
        .to_string();
    let live = crate::client::rpc(&state, "board_session_check", json!({"token": token}))
        .expect("the daemon could not check the minted session");
    assert_eq!(
        live["valid"], true,
        "the human's minted session is not live — every refusal below would be vacuous"
    );
    assert_eq!(live["session"]["user"]["sub"], "alice");

    // ---- POSITIVE CONTROL: the same request from the human ----------------
    // The test's own process is tied to no registered agent, so the board
    // admits it exactly as it admits the human's browser, and the real
    // member surface answers with the bound assistant.
    rig.authorize_as("alice");
    let mut admitted = rig.member_post(
        "/api/member-assistant/state",
        Some(cookie.as_str()),
        &json!({}),
    );
    assert_eq!(
        admitted.status().as_u16(),
        200,
        "the human's own member-assistant request was refused — the positive control no longer passes this route"
    );
    let admitted: Value = admitted.body_mut().read_json().unwrap();
    assert_eq!(
        admitted["assistant"]["alias"], PRIVATE_ALIAS,
        "the positive control resolved another assistant: {admitted}"
    );
    rig.assert_authority_wire(&token, "state");

    // ---- The private member-assistant PROCESS -----------------------------
    // A separate OS process that has not connected yet; its pid and start
    // time are recorded exactly as the daemon records a live pane endpoint.
    let private = spawn_private_assistant(&rig, &token);
    let pid = private.id();
    let pid_start = crate::peer::proc_starttime(pid)
        .expect("the private process has no /proc start time to record");

    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(&state, &options)
        .expect("the fixture cannot open a second daemon view");
    {
        let conn = shared.store.try_conn().expect("the fixture store is busy");
        // Another connection writes the row the daemon records on open; it
        // may briefly contend with the live daemon's writer.
        let _ = conn.busy_timeout(Duration::from_secs(10));
        let changed = conn
            .execute(
                "UPDATE agents SET endpoint_kind='pty', generation=?, pid=?, \
                 pid_start=?, endpoint=? WHERE alias=?",
                rusqlite::params![
                    PANE_GENERATION,
                    pid as i64,
                    pid_start as i64,
                    format!("pty:{pid}"),
                    PRIVATE_ALIAS,
                ],
            )
            .expect("the fixture cannot record the private process as its assistant's pane");
        assert_eq!(
            changed, 1,
            "the positive control did not register the assistant runtime row this fixture binds"
        );
    }
    shared
        .store
        .register_agent(&NewAgent {
            alias: ORDINARY_ALIAS,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture cannot register the ordinary control row");

    // ---- The bypass, observed against the real daemon ---------------------
    // `agent_list` is the profile-bearing list a board might still attribute
    // from: as the operator it must NOT carry the member's private row. The
    // narrow operational projection must carry it (pid included), or no
    // board could ever tie the process to that assistant.
    let list = scoped(Asserted::Operator, || {
        crate::client::rpc(&state, "agent_list", json!({}))
    })
    .expect("the operator's agent list was refused");
    let listed = aliases(&list);
    assert!(
        listed.iter().any(|alias| alias == ORDINARY_ALIAS),
        "the operator list lost the ordinary control row — a blanket list cannot stand in for the private-row filter: {listed:?}"
    );
    assert!(
        !listed.iter().any(|alias| alias == PRIVATE_ALIAS),
        "agent_list still carries the member's private assistant row: {listed:?}"
    );

    let operational = scoped(Asserted::Operator, || {
        crate::client::rpc(&state, "agent_operational", json!({}))
    })
    .expect("the operator's operational projection was refused");
    let operational_aliases = aliases(&operational);
    assert!(
        operational_aliases
            .iter()
            .any(|alias| alias == ORDINARY_ALIAS),
        "the operational projection lost the ordinary row: {operational_aliases:?}"
    );
    let row = operational["agents"]
        .as_array()
        .expect("the operational projection is not a list")
        .iter()
        .find(|agent| agent.get("alias").and_then(Value::as_str) == Some(PRIVATE_ALIAS))
        .unwrap_or_else(|| {
            panic!(
                "the operational projection lost the private assistant's pid→alias row — a board attributing from agent_list would read its process as anonymous: {operational}"
            )
        });
    assert_eq!(
        row["pid"].as_u64(),
        Some(u64::from(pid)),
        "the operational projection names another pid: {row}"
    );
    assert_eq!(row["pid_start"].as_u64(), Some(pid_start), "{row}");
    assert_eq!(row["endpoint_kind"], "pty", "{row}");
    assert_eq!(row["generation"], PANE_GENERATION, "{row}");

    // ---- THE BAD CASE -----------------------------------------------------
    // The real private assistant process now sends the same member POST with
    // the human's live cookie from its own socket.
    let (status, body) = drive(private);
    assert_ne!(
        status, 200,
        "the private member-assistant process used the human's cookie to reach member authority: {body}"
    );
    assert_eq!(
        status, 403,
        "the board did not answer the caller-attribution refusal (a refusal for an unreadable dependency is not this check): {body}"
    );
    assert!(
        body.get("assistant").is_none(),
        "a refusal returned member assistant state: {body}"
    );
    // `Verdict::Stolen` is the ONLY branch that revokes the presented token.
    // A live token here means the board never named the process as the
    // registered assistant (the filtered-list bypass, or an unknown caller),
    // whatever status it happened to answer.
    let revoked = crate::client::rpc(&state, "board_session_check", json!({"token": token}))
        .expect("the daemon could not re-check the session after the refusal");
    assert_eq!(
        revoked["valid"], false,
        "the board did not attribute the request to the registered private assistant: the presented human session was left live (the agent-list-filter bypass)"
    );
}
