//! ACCEPTANCE CHECK (CAD-1123 HP2+HP3), author: cc13-pi-acc792,
//! implementer may not edit.
//!
//! Written from the ticket (`cadence issue show CAD-1123`, the HP2/HP3
//! sections of `claudedocs/cad1123-build-plan-20261003.md` and AGENTS.md
//! "Gates and security work"), not from the implementation. Each test
//! exercises a real guard — the daemon `Shared::dispatch` under the
//! test-seam, and the board's write path over real HTTP — asserts the
//! refusal by its reason, and asserts nothing was written. Every check
//! also holds a positive control so it cannot pass by refusing
//! everything.
//!
//! The six cases the ticket asks to be refused:
//!   1. spend without the operator (agent caller, unproven caller, RPC
//!      and HTTP): `app_run_start` / `POST /api/app-runs/start` refused,
//!      no run row, no dispatch;
//!   2. a forged team (a team-role input or an owner/assignments field
//!      in the request) and a missing team;
//!   3. a changed `expected_quotes` -> `price_changed`;
//!   4. a replayed `request_id` -> the same run, one dispatch;
//!   5. `app_install_team_set` operator-only (RPC and HTTP), a stale
//!      compare-and-swap refused;
//!   6. HTTP at least as strict as the RPC: `RouteClass::OperatorOnly`
//!      in `WRITE_ROUTES`, an operator relay, a 400 for a shape the RPC
//!      never sees.
//!
//! Guards exercised (for the mutation record):
//!   - `Shared::operator_connection` on `rpc_app_local` /
//!     `rpc_app_team` (the `caller_rule` rows for `app_run_start`,
//!     `app_install_team_set`, `app_install_team_show`);
//!   - `WRITE_ROUTES` OperatorOnly rows for `/api/app-runs/start` and
//!     `/api/app-installations/*/team` + `write_route`'s fail-closed
//!     default and `admit_operator_read` for the team GET;
//!   - the role-forgery refusal and the `no default team` refusal in
//!     `Shared::create_app_run`/`start_app_run` (app_runs_rpc.rs);
//!   - the `expected_quotes` frozen-quote comparison (price_changed);
//!   - `UNIQUE(install_id, request_id)` replay in
//!     `Store::app_run_create_with_capabilities`;
//!   - the `expected_revision` compare-and-swap in `rpc_app_team`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::cad1120_tests::{refusal, Fx};
use super::*;
use crate::test_seam::{scoped, Asserted, Seam};

/// Count the run rows of the fixture's install — the "was anything
/// written" assertion for refusals that must create no run — through
/// the store's own list (no raw SQL: the writer census counts opens).
fn runs_of(fx: &Fx) -> i64 {
    fx.shared
        .store
        .app_run_list_filtered(Some(&fx.install), None)
        .unwrap()["runs"]
        .as_array()
        .map_or(0, |r| r.len() as i64)
}

fn team_set(fx: &Fx, expected_revision: u64) -> Result<Value> {
    fx.operator(
        "app_install_team_set",
        json!({"install_id": fx.install, "owner_pm": "lead",
            "roles": {"writer": "writer", "reviewer": "reviewer"},
            "expected_revision": expected_revision}),
    )
}

fn start_params(fx: &Fx, request: &str) -> Value {
    json!({
        "install_id": fx.install,
        "workflow": "email-brief",
        "request_id": request,
        "expected_quotes": {},
        "inputs": {
            "subject": "Renewal",
            "audience": "Customers due a renewal",
            "facts": "Plan renews 1 July. Price stays HK$88/month.",
        },
    })
}

/// The writer step's dispatch proof: a queued kickoff message for the
/// run's first step. `None` while the run was never dispatched.
fn writer_kickoff(fx: &Fx, run: &Value) -> Option<String> {
    fx.shared
        .store
        .app_run_show(run["id"].as_str().unwrap())
        .unwrap()["steps"][0]["message_id"]
        .as_str()
        .map(str::to_string)
}

/// Case 1 (RPC): `app_run_start` is an operator action. An agent pane
/// and a provably-no-one (detached) caller are refused by
/// `operator_connection`, and no run row exists afterwards. Control:
/// the same call as the operator creates, approves and dispatches.
#[test]
fn accept_spend_needs_the_operator_on_the_rpc() {
    let fx = Fx::new();
    fx.start_team();
    team_set(&fx, 0).unwrap();
    for who in [
        Asserted::Agent("writer".into()),
        Asserted::Agent("lead".into()),
        Asserted::Unproven,
    ] {
        let err = refusal(fx.call(who.clone(), "app_run_start", start_params(&fx, "n-os")));
        assert!(
            err.contains("operator") || err.contains("unproven"),
            "{who:?}: {err}"
        );
    }
    assert_eq!(runs_of(&fx), 0, "a refused start wrote a run row");
    // Control: the operator's start reaches running (a parked worker
    // would be refused at creation — the enabled-team check stands).
    let run = fx
        .operator("app_run_start", start_params(&fx, "n-os"))
        .unwrap();
    assert_eq!(run["state"], "running", "{run}");
    assert!(writer_kickoff(&fx, &run).is_some());
}

/// Case 5 (RPC): both team verbs are operator-only for an agent and an
/// unproven caller; the stored team is unchanged afterwards.
#[test]
fn accept_team_verbs_are_operator_only_on_the_rpc() {
    let fx = Fx::new();
    team_set(&fx, 0).unwrap();
    for who in [Asserted::Agent("writer".into()), Asserted::Unproven] {
        for (method, params) in [
            (
                "app_install_team_set",
                json!({"install_id": fx.install, "owner_pm": "lead",
                    "roles": {"writer": "writer"}, "expected_revision": 1}),
            ),
            ("app_install_team_show", json!({"install_id": fx.install})),
        ] {
            let err = refusal(fx.call(who.clone(), method, params));
            assert!(
                err.contains("operator") || err.contains("unproven"),
                "{who:?} {method}: {err}"
            );
        }
    }
    let shown = fx
        .operator("app_install_team_show", json!({"install_id": fx.install}))
        .unwrap();
    assert_eq!(shown["team"]["revision"], json!(1), "a team was written");
}

/// Case 2: the request can never name the team. A team-role input
/// (`writer`, `reviewer`), an `owner_pm` or an `assignments` field is a
/// forgery and is refused; a missing team is refused. Each refusal
/// leaves no run row. Control: with the team set and none of these in
/// the request, the same body is served.
#[test]
fn accept_no_forged_team_and_no_missing_team() {
    let fx = Fx::new();
    fx.start_team();
    // Missing team: refused before any work, no row.
    let err = refusal(fx.operator("app_run_start", start_params(&fx, "m-t")));
    assert!(err.contains("no default team"), "{err}");
    assert_eq!(runs_of(&fx), 0);
    team_set(&fx, 0).unwrap();
    // A supplied value for a team-role input is a forgery — including a
    // value identical to the stored team's (the key itself is forged).
    for key in ["writer", "reviewer"] {
        let mut forged = start_params(&fx, &format!("forge-{key}"));
        forged["inputs"][key] = json!("writer"); // the team's own writer: still forged
        let err = refusal(fx.operator("app_run_start", forged));
        assert!(err.contains("installation team"), "{key}: {err}");
    }
    // Owner, project, assignment and approval fields can never be sent:
    // the payload allowlist refuses them before anything runs.
    for (field, value) in [
        ("owner_pm", json!("lead")),
        ("project_link", json!("x")),
        ("assignments", json!({})),
        ("digest", json!("sha256:x")),
        ("roles", json!({"writer": "writer"})),
    ] {
        let mut params = start_params(&fx, &format!("forge-f-{field}"));
        params[field] = value;
        let err = refusal(fx.operator("app_run_start", params));
        assert!(err.contains("unsupported fields"), "{field}: {err}");
    }
    assert_eq!(runs_of(&fx), 0, "a forged start wrote a run row");
    // Control: the honest request is served.
    let run = fx
        .operator("app_run_start", start_params(&fx, "honest"))
        .unwrap();
    assert_eq!(run["state"], "running", "{run}");
    assert_eq!(run["snapshot"]["inputs"]["writer"], json!("writer"));
    assert_eq!(run["snapshot"]["owner_pm"], json!("lead"));
}

/// Case 3: `expected_quotes` must equal the quotes the run freezes —
/// any difference (an extra slot, a missing map) is `price_changed` —
/// and nothing is created. Control: the exact frozen set (`{}` for a
/// workflow with no capability slots) is served.
#[test]
fn accept_price_drift_is_refused_with_price_changed() {
    let fx = Fx::new();
    fx.start_team();
    team_set(&fx, 0).unwrap();
    for expected in [
        json!({"image": {"schema": 1, "quote": {"amount_minor": 1}}}),
        json!({"ghost": null}),
    ] {
        let mut params = start_params(&fx, "drift");
        params["expected_quotes"] = expected;
        let err = refusal(fx.operator("app_run_start", params));
        assert!(err.contains("price_changed"), "{err}");
    }
    let mut missing = start_params(&fx, "drift-missing");
    missing.as_object_mut().unwrap().remove("expected_quotes");
    assert!(refusal(fx.operator("app_run_start", missing)).contains("expected_quotes"));
    assert_eq!(runs_of(&fx), 0, "a price-drifted start wrote a run row");
    let run = fx
        .operator("app_run_start", start_params(&fx, "exact"))
        .unwrap();
    assert_eq!(run["state"], "running", "{run}");
}

/// Case 4: a replayed `request_id` returns the same run row and never
/// dispatches twice — the guard is `UNIQUE(install_id, request_id)` in
/// the store. A different request id with the same body is a different
/// run (no false dedup). The team is parked (auto-idle-stopped), so the
/// dispatch that never replays is also the CAD-1120 wake path: the first
/// call wakes the writer, the replay must not wake it a second time.
#[test]
fn accept_replayed_request_id_returns_the_same_run_and_dispatches_once() {
    let fx = Fx::new();
    fx.start_team();
    team_set(&fx, 0).unwrap();
    fx.idle_out();
    let run = fx
        .operator("app_run_start", start_params(&fx, "replay"))
        .unwrap();
    assert_eq!(run["state"], "running", "{run}");
    let kickoff = writer_kickoff(&fx, &run).expect("writer kickoff queued");
    // The dispatch woke the parked writer (CAD-1120: no manual resume):
    // the kickoff delivers to a live, enabled, owned worker.
    fx.wait("writer woken by the start's dispatch", || {
        fx.shared
            .store
            .message(&kickoff)
            .unwrap()
            .is_some_and(|m| m.turn_id.is_some())
    });
    let writer = fx.agent("writer");
    assert!(writer.enabled && fx.owned("writer"));
    assert_ne!(
        fx.marker("writer"),
        AUTO_STOP_EVENT,
        "the writer stayed parked"
    );
    // Byte-identical replay: same row, same state, no second kickoff.
    let again = fx
        .operator("app_run_start", start_params(&fx, "replay"))
        .unwrap();
    assert_eq!(again["id"], run["id"]);
    assert_eq!(runs_of(&fx), 1, "the replay made a second run row");
    // The replayed run shows the SAME kickoff message, not a new one.
    let shown = fx
        .shared
        .store
        .app_run_show(run["id"].as_str().unwrap())
        .unwrap();
    assert_eq!(shown["steps"][0]["message_id"], json!(kickoff));
    // A different request id is a different run.
    let other = fx
        .operator("app_run_start", start_params(&fx, "replay-2"))
        .unwrap();
    assert_ne!(other["id"], run["id"]);
    assert_eq!(runs_of(&fx), 2);
}

/// Case 5: the team write is a compare-and-swap — a stale
/// `expected_revision` is refused and the stored team does not move.
/// Control: the current revision is served.
#[test]
fn accept_stale_team_compare_and_swap_is_refused() {
    let fx = Fx::new();
    team_set(&fx, 0).unwrap();
    // Stale: the stored revision is 1; replaying 0 must lose.
    let err = refusal(team_set(&fx, 0));
    assert!(err.contains("stale"), "{err}");
    // Missing and non-numeric expectations are refused too.
    for params in [
        json!({"install_id": fx.install, "owner_pm": "lead",
            "roles": {"writer": "writer"}}),
        json!({"install_id": fx.install, "owner_pm": "lead",
            "roles": {"writer": "writer"}, "expected_revision": "1"}),
    ] {
        assert!(fx.operator("app_install_team_set", params).is_err());
    }
    let shown = fx
        .operator("app_install_team_show", json!({"install_id": fx.install}))
        .unwrap();
    assert_eq!(shown["team"]["revision"], json!(1));
    assert_eq!(shown["team"]["roles"]["reviewer"], json!("reviewer"));
    // Control: the current revision swaps, and the new revision answers.
    let set = team_set(&fx, 1).unwrap();
    assert_eq!(set["team"]["revision"], json!(2), "{set}");
}

// ---------------- the board's real HTTP peer ----------------

/// A fixture board over a real daemon fixture, mirroring the
/// conversations_acceptance `http` harness: seam-armed daemon +
/// seam-attached board on 3110–3199, an operator session minted the
/// real way.
mod http {
    use super::*;

    pub(super) struct Stop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
            for thread in self.1.drain(..).rev() {
                let _ = thread.join();
            }
        }
    }

    pub(super) struct Board {
        agent: ureq::Agent,
        pub(super) base: String,
        host: String,
        token: String,
        cookie: String,
        key: String,
        pub(super) _stop: Stop,
    }

    impl Board {
        fn decorate<B>(&self, who: &str, b: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
            let b = b
                .header("Host", &self.host)
                .header("X-Cadence-Board", "1")
                .header("Origin", format!("http://{}", self.host))
                .header(crate::test_seam::AS_HEADER, who)
                .header(crate::test_seam::TOKEN_HEADER, &self.token);
            if who == "operator" {
                b.header("Cookie", &self.cookie)
                    .header("X-Cadence-Session", &self.key)
            } else {
                b
            }
        }

        /// `who`: `operator` (with the board session), `agent:<alias>`
        /// or `unproven`. Returns (status, json body).
        pub(super) fn call(
            &self,
            who: &str,
            method: &str,
            path: &str,
            body: Option<Value>,
        ) -> (u16, Value) {
            let url = format!("{}{path}", self.base);
            let mut response = match (method, body) {
                ("GET", _) => self.decorate(who, self.agent.get(&url)).call(),
                (_, body) => self
                    .decorate(who, self.agent.post(&url))
                    .header("Content-Type", "application/json")
                    .send(body.unwrap_or(Value::Null).to_string()),
            }
            .unwrap();
            let status = response.status().as_u16();
            let body = response.body_mut().read_json().unwrap_or(Value::Null);
            (status, body)
        }
    }

    /// Start a seam daemon over `dir` and a board; mint an operator
    /// session through `operator_link_mint` + `POST /api/session`.
    pub(super) fn start(dir: &std::path::Path) -> Board {
        let state = dir.to_path_buf();
        let pm = state.join("pm");
        let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "daemon never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut port = 3110 + (std::process::id() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let board = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.0.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.clone(), pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => break stop.1.push(thread),
                Err(_) if port < 3199 => port += 1,
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
            thread.join().unwrap();
        }
        let token = Seam::token_at(&state).unwrap();
        let host = format!("cadence-{port}.localhost:{port}");
        crate::operator_auth::ensure_secret(&state).unwrap();
        let secret = crate::operator_auth::read_secret(&state).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let config = ureq::Agent::config_builder().http_status_as_error(false);
        let agent: ureq::Agent = config.build().into();
        let base = format!("http://127.0.0.1:{port}");
        let session = agent
            .post(format!("{base}/api/session"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let set = session.headers()["set-cookie"].to_str().unwrap();
        let cookie = set[..set.find(';').unwrap()].to_owned();
        let key: Value = session.into_body().read_json().unwrap();
        Board {
            agent,
            base,
            host,
            token,
            cookie,
            key: key["session_key"].as_str().unwrap().to_string(),
            _stop: stop,
        }
    }
}

/// Cases 1, 5 and 6 on the HTTP peer: every new write route refuses an
/// agent-asserted and a sessionless request at the board's write gate
/// (`check: operator_only`, 403) before any relay, nothing is written,
/// and the team read refuses them the same way. Controls: the
/// operator's session reaches the daemon on every route — start gets a
/// parked team to `running` through the board (the dispatch wakes the
/// workers it needs, CAD-1120), the team write swaps, the team read
/// serves the stored team — and a forged team-role key in the body is
/// refused by the daemon through the board relay.
#[test]
fn accept_board_http_is_at_least_as_strict_as_the_rpc() {
    let fx = Fx::new();
    fx.start_team();
    team_set(&fx, 0).unwrap();
    // Park the team: `Fx`'s drop is then a no-op for them, and the HTTP
    // start's dispatch is the real wake path the board drives. `fx`
    // stays alive for the test: its TempDir IS the state dir.
    fx.idle_out();
    let dir = fx.state();
    let install = fx.install.clone();
    let board = http::start(&dir);
    let start_body = json!({
        "install_id": install,
        "workflow": "email-brief",
        "request_id": "http-start",
        "expected_quotes": {},
        "inputs": {"subject": "Renewal", "audience": "Customers",
                   "facts": "Plan renews 1 July."},
    });
    let team_path = format!("/api/app-installations/{install}/team");
    let team_body =
        json!({"owner_pm": "lead", "roles": {"writer": "writer"}, "expected_revision": 1});
    for who in ["agent:writer", "unproven"] {
        for (method, path, body) in [
            (
                "POST",
                "/api/app-runs/start".to_string(),
                Some(start_body.clone()),
            ),
            ("POST", team_path.clone(), Some(team_body.clone())),
            ("GET", team_path.clone(), None),
        ] {
            let (status, reply) = board.call(who, method, &path, body);
            assert_eq!(status, 403, "{who} {method} {path}: {reply}");
            // An agent's attributed write is refused as the operator's
            // own; an unattributable one cannot even name a caller —
            // either way it is refused before any daemon relay.
            assert!(
                matches!(
                    reply["check"].as_str(),
                    Some("operator_only" | "caller_identity")
                ),
                "{who} {method} {path}: {reply}"
            );
        }
    }
    // A forged team-role key inside `inputs` rides the operator's own
    // session: the board cannot see it, the DAEMON must refuse it. The
    // refusal reaches the frame as a non-2xx, and no run is written.
    let mut forged = start_body.clone();
    forged["inputs"]["writer"] = json!("writer");
    forged["request_id"] = json!("http-forged");
    let (status, reply) = board.call("operator", "POST", "/api/app-runs/start", Some(forged));
    assert!(status >= 400, "a forged team key was relayed: {reply}");
    assert_eq!(runs_of(&fx), 0, "a refused HTTP write created a run");
    // Controls: the operator's session is served on every route.
    let (status, reply) = board.call("operator", "POST", "/api/app-runs/start", Some(start_body));
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["state"], json!("running"), "{reply}");
    assert_eq!(reply["snapshot"]["inputs"]["writer"], json!("writer"));
    assert_eq!(runs_of(&fx), 1);
    let (status, reply) = board.call("operator", "GET", &team_path, None);
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["team"]["revision"], json!(1), "{reply}");
    let (status, reply) = board.call("operator", "POST", &team_path, Some(team_body));
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["team"]["revision"], json!(2), "{reply}");
    // And a stale team swap over HTTP loses the same compare-and-swap.
    let (status, reply) = board.call(
        "operator",
        "POST",
        &team_path,
        Some(json!({"owner_pm": "lead", "roles": {"writer": "writer"}, "expected_revision": 1})),
    );
    assert!(status >= 400, "a stale swap was relayed: {reply}");
}
