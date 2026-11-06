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
//! production `/proc` one: the member runtime's own managed `pi` endpoint row
//! is given the process exactly as the daemon records a launched endpoint —
//! `Store::set_identity` inside the guarded writer lane (alias, pid, the
//! pid's `/proc` start time, a generation) — never a raw store write. No
//! `X-Cadence-Test-As` assertion header and no caller-chosen environment
//! alias is used as identity evidence anywhere in this check.
//!
//! The contract this pins: a member's private assistant row — and its
//! metadata — is withheld from BOTH operator projections (`agent_list` and
//! the narrow `agent_operational`), while ordinary workers stay visible. A
//! board that built its pid→alias attribution roots from either projection
//! would stop recognising the assistant's process, read the human cookie as
//! the human's own, and relay member authority to the assistant. The board
//! therefore attributes the live TCP four-tuple through the daemon's
//! all-agent classifier, which reports an opaque generic agent for a private
//! caller and never its alias. Both halves are asserted against the REAL
//! daemon: the private row is absent from both operator projections while the
//! ordinary row stays present, and the bad HTTP request is then refused at
//! `board_caller` with the presented session revoked — the only effect of the
//! board placing the caller as an agent.
//!
//! The positive control runs first on the same fixture, route, cookie and
//! fixture authority: the human's own request (from a process tied to no
//! agent) is admitted and reaches the member surface, so a blanket refusal
//! cannot pass these checks.
//!
//! Also pinned here, because it is the same attribution problem: the
//! `daemon restart --when-idle` gate. The CLI (`src/cli/daemon.rs`) asks the
//! daemon's `agent_fleet_state`, which must see a member assistant's
//! in-flight turn as BUSY and must refuse to claim the fleet is resumable
//! under operator authority — while never naming the assistant. This file
//! asserts that aggregate against the real daemon; it does not spawn the CLI
//! process, and the owner-signed resume outcome (the member path that may
//! report the assistant's real state) is not exercised here.

use super::super::{start, Rig};
use crate::adapter::Identity;
use crate::daemon::team_assistants_refusal::AssistantFlagOn;
use crate::daemon::ServeOptions;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Child, Command, Stdio};

/// The alias the member's stored binding names (the fixture's
/// `start(&["alice"])` binds exactly this), asserted by the positive control
/// before any attribution decision below.
const PRIVATE_ALIAS: &str = "assist-agent-alice";
/// The endpoint generation recorded with the process, exactly as an adapter
/// `open` records one. It is fixture bookkeeping for the store's endpoint row;
/// the board's four-tuple attribution uses the pid, not a generation.
const ENDPOINT_GENERATION: &str = "cad2207-fixture-generation";
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
/// only claim to a caller is the managed endpoint row the daemon records
/// below.
fn spawn_private_assistant(rig: &Rig, token: &str) -> Child {
    let mut command = Command::new("/bin/bash");
    command
        .arg("-s")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOST", &rig.public_host)
        .env("PORT", rig.board_port.to_string())
        .env("COOKIE", token)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::reaper::spawn(&mut command)
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
    // A separate OS process that has not connected yet. The member's runtime
    // row — registered by the positive control as provider `pi`, endpoint kind
    // `managed`, exactly the runtime a bound member assistant gets — is given
    // that process through the store's own guarded `set_identity`, the same
    // API an adapter `open` uses: it records the pid, the pid's `/proc` start
    // time and a generation inside the arming writer lane. No identity is
    // written through raw SQL: the busy-turn row later is seeded through the
    // store's test-only `fixture_write` façade, which is the supported seam
    // and still refuses a sealed store.
    let private = spawn_private_assistant(&rig, &token);
    let pid = private.id();

    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(&state, &options)
        .expect("the fixture cannot open a second daemon view");
    let runtime = shared
        .store
        .agent_opt(PRIVATE_ALIAS)
        .expect("the fixture store is busy")
        .expect(
            "the positive control did not register the assistant runtime row this fixture binds",
        );
    shared
        .store
        .set_identity(
            PRIVATE_ALIAS,
            &Identity {
                thread_id: runtime.thread_id.clone().unwrap_or_default(),
                session_id: runtime.session_id.clone().unwrap_or_default(),
                model: runtime.model.clone(),
                effort: runtime.effort.clone(),
                pid,
                endpoint: None,
                generation: Some(ENDPOINT_GENERATION.to_string()),
                attach: None,
            },
        )
        .expect("the fixture cannot record the private process as the member runtime's managed endpoint");
    shared
        .store
        .register_agent(&NewAgent {
            alias: ORDINARY_ALIAS,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: state.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture cannot register the ordinary control row");

    // ---- The privacy projection, observed against the real daemon ---------
    // The agreed public contract: an operator sees ordinary workers in both
    // the profile-bearing list and the narrow operational projection, but a
    // member's private assistant row — alias, pid or any other metadata — is
    // absent from both. The board's four-tuple attribution therefore cannot
    // ride either projection; the real refusal below proves the daemon's
    // all-agent classifier still places the live process as an agent.
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
    assert!(
        !operational_aliases.iter().any(|alias| alias == PRIVATE_ALIAS),
        "agent_operational still carries the member's private assistant row: {operational_aliases:?}"
    );
    assert!(
        !operational.to_string().contains(PRIVATE_ALIAS),
        "agent_operational still carries the private assistant's alias or metadata: {operational}"
    );

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
    // `Verdict::Stolen`/`StolenOpaque` are the only branches that revoke the
    // presented token. A live token here means the board never placed the
    // process as an agent (a projection-only attribution, or an unknown
    // caller), whatever status it happened to answer.
    let revoked = crate::client::rpc(&state, "board_session_check", json!({"token": token}))
        .expect("the daemon could not re-check the session after the refusal");
    assert_eq!(
        revoked["valid"], false,
        "the board did not place the private process as an agent: the presented human session was left live (a projection-only attribution cannot satisfy the four-tuple classifier)"
    );
}

/// CAD-2207: `daemon restart --when-idle` must observe a member assistant's
/// in-flight turn even though its row is withheld from every operator
/// projection. The CLI's gate is the daemon's `agent_fleet_state`; this
/// exercises that aggregate directly against a real daemon, with a live
/// private process recorded exactly as an adapter `open` records one. It
/// asserts the aggregate the CLI consumes — busy, the resume verdict, and no
/// alias — not only the HTTP refusal above.
#[test]
fn when_idle_gate_sees_a_busy_private_assistant_without_naming_it() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["alice"]);
    let state = rig._dir.path().to_path_buf();
    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(&state, &options)
        .expect("the fixture cannot open a daemon view");

    // The fixture's provisioning stored the member binding; register the
    // runtime row the daemon lazily creates on the member's first call.
    let cwd = state.join("cad2207-assistant-cwd");
    std::fs::create_dir_all(&cwd).expect("the fixture cannot create the assistant cwd");
    shared
        .store
        .register_agent(&NewAgent {
            alias: PRIVATE_ALIAS,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("the fixture cannot register the bound assistant's runtime row");

    // A live process, recorded as the runtime's managed endpoint exactly as
    // an adapter `open` records it: pid, `/proc` start time, generation.
    let mut private_command = Command::new("/bin/bash");
    private_command
        .arg("-s")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut private = crate::reaper::spawn(&mut private_command)
        .expect("the fixture cannot spawn a private assistant process");
    shared
        .store
        .set_identity(
            PRIVATE_ALIAS,
            &Identity {
                thread_id: String::new(),
                session_id: String::new(),
                model: None,
                effort: None,
                pid: private.id(),
                endpoint: None,
                generation: Some(ENDPOINT_GENERATION.to_string()),
                attach: None,
            },
        )
        .expect("the fixture cannot record the private process as the runtime's managed endpoint");

    // Mid-launch with one in-flight turn: the live blocker `--when-idle`
    // must observe. The turn row is seeded through the store's test-only
    // write hook, not a production writer.
    const BUSY_TURN: &str = "cad2207-busy-turn";
    shared
        .store
        .set_agent_state(PRIVATE_ALIAS, "starting", None)
        .expect("the fixture cannot put the private assistant mid-launch");
    shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT INTO messages(id,alias,body,reply_to,source,state,created) \
                 VALUES(?,?,?,NULL,'member','running',?)",
                rusqlite::params![
                    BUSY_TURN,
                    PRIVATE_ALIAS,
                    "in-flight private turn",
                    crate::issue::time::now_epoch() as f64
                ],
            )?;
            Ok(())
        })
        .expect("the fixture cannot seed the private assistant's in-flight turn");

    let fleet = scoped(Asserted::Operator, || {
        crate::client::rpc(&state, "agent_fleet_state", json!({}))
    })
    .expect("the operator's fleet-state gate was refused");
    assert_eq!(
        fleet["busy"], true,
        "a busy member assistant is invisible to the restart gate — \
         `daemon restart --when-idle` would restart mid-turn: {fleet}"
    );
    assert_eq!(
        fleet["complete"], false,
        "the resume verdict claims a restricted runtime can be resumed under \
         operator authority: {fleet}"
    );
    assert!(
        !fleet.to_string().contains(PRIVATE_ALIAS),
        "the restart gate named the member's private assistant: {fleet}"
    );

    // A caller-supplied ignored pair never exempts a private in-flight turn:
    // even when the operator submits this assistant's REAL (alias,
    // message_id), the turn still blocks the restart. The answer again names
    // neither identifier — the pair is the caller's own input, never an
    // echoed identity.
    let ignored = scoped(Asserted::Operator, || {
        crate::client::rpc(
            &state,
            "agent_fleet_state",
            json!({"ignored_stale": [[PRIVATE_ALIAS, BUSY_TURN]]}),
        )
    })
    .expect("the operator's fleet-state gate was refused with an ignored pair");
    assert_eq!(
        ignored["busy"], true,
        "an operator-supplied ignored pair exempted a private in-flight turn — \
         `daemon restart --when-idle --ignore-stale` would restart mid-turn: {ignored}"
    );
    assert_eq!(
        ignored["complete"], false,
        "the resume verdict changed when the operator named a private pair: {ignored}"
    );
    assert!(
        !ignored.to_string().contains(PRIVATE_ALIAS) && !ignored.to_string().contains(BUSY_TURN),
        "the restart gate echoed a private identifier: {ignored}"
    );

    let _ = private.kill();
    let _ = private.wait();
}
