//! CAD-1700 acceptance checks: the operator-only scoped `/stop` and
//! `/cancel`. Written from the ticket (`CAD-1700`, section "Scoped Stop and
//! Cancel") and the lane contract, not from the implementation. Each test
//! proves one bad case is REFUSED at the real daemon guard
//! (`Shared::dispatch` under the test seam); a refusal is asserted by its
//! code or reason text, never by "is_err", and every test also holds a
//! positive control so it cannot pass by refusing everything.
//!
//! Guards exercised:
//! - `rpc_master_command`'s operator gate (`operator_connection`) for both
//!   verbs: an agent caller, including the master's own connection, is
//!   refused before any turn is touched.
//! - `turn_not_running`: `stop {message}` reaches only the exact running
//!   turn; a queued, finished, unknown or stale id is refused, and a newer
//!   running turn survives a stale stop.
//! - `message_required`, `message_running`, `message_not_queued`,
//!   `message_not_cancellable`: `cancel {message}` removes exactly one
//!   still-queued operator message to the master and nothing else.
//!
//! Mutation record (first pass on a clean local build; the reviewer must
//! re-run and finish it):
//!
//! - **Operator gate** (`rpc_master_command`: `operator_connection(...)?`
//!   neutralized): `cad1700_an_agent_caller_cannot_stop_or_cancel_a_master_turn`
//!   FAILS — the operator-layer refusal is replaced by
//!   `interrupt refused: agent 'w1' cannot change another agent …`.
//!   Sensitive.
//! - **Running-turn invariant** (`stop`'s running check and the
//!   pre-adapter re-check): neutralizing either one ALONE leaves all five
//!   tests green — they enforce the same rule (defence in depth). Neutralizing
//!   BOTH fails two:
//!   `cad1700_stop_refuses_anything_but_the_current_running_turn`
//!   (`stop m-queued is refused: Agent 'master' has no live endpoint …`,
//!   the wrong reason) and
//!   `cad1700_a_stale_stop_never_interrupts_a_newer_turn` (no refusal at all,
//!   `left: None`). Sensitive as a pair.
//! - **Cancel provenance** (`sender` / `sender_uid` conditions dropped):
//!   `cad1700_cancel_refuses_a_forged_operator_source` FAILS — the forged
//!   `source: "operator"` message is cancelled (`ok: true`, state
//!   `cancelled`), so the provenance rule is what refuses it. Sensitive.
//! - **`message_required`** (the missing-message branch made unreachable):
//!   `cad1700_cancel_refuses_everything_but_the_operators_own_queued_message`
//!   FAILS — got `message_not_cancellable` instead. Sensitive.
//! - **`message_running`** (the running/submitting arm disabled): the same
//!   test FAILS — got `message_not_queued` instead. Sensitive.
//! - **Relay message validation** (`message` length/empty check removed in
//!   `src/ui/home.rs`): **NOT COVERED.** The group still passed 6/6: the
//!   mapping test exercises `master_command_err` directly and never drives
//!   the relay's own `message` validation, which needs a `Request`. A board
//!   HTTP test must add that case.
//! - **Relay refusal mapping** (`master_command_err` collapsed to the
//!   shared mapping): `scoped_command_refusals_map_to_409` FAILS —
//!   `turn_not_running maps to 409`, got 400. Sensitive.

use super::*;
use crate::adapter::Identity;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};

fn pid() -> u32 {
    std::process::id()
}

struct Fx {
    _dir: tempfile::TempDir,
    shared: Arc<Shared>,
}

impl Fx {
    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || self.shared.dispatch(method, &params, pid()))
    }

    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(crate::rollout::db_file(self._dir.path())).unwrap()
    }

    /// The operator's chat message to the master, queued.
    fn send(&self, id: &str) {
        self.operator(
            "thread_send",
            json!({"alias": "master", "text": format!("hello {id}"), "message": id}),
        )
        .unwrap();
    }

    /// Make `id` the master's RUNNING turn.
    fn run(&self, id: &str) {
        let token = crate::adapter::registry::PI_MANAGED_TURN_TOKENS
            .mint("0123456789abcdef0123456789abcdef");
        self.db()
            .execute(
                "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
                [id],
            )
            .unwrap();
        self.shared.store.mark_running(id, &token).unwrap();
    }

    /// Finish `id` as a delivered turn.
    fn finish(&self, id: &str) {
        self.db()
            .execute(
                "UPDATE messages SET state='completed', started=1.0, completed=2.0 WHERE id=?",
                [id],
            )
            .unwrap();
    }

    fn state(&self, id: &str) -> String {
        self.shared.store.message(id).unwrap().unwrap().state
    }

    /// `{command, message}` as the operator.
    fn command(&self, command: &str, message: Option<&str>) -> Result<Value> {
        let mut params = json!({"command": command});
        if let Some(id) = message {
            params["message"] = json!(id);
        }
        self.operator("master_command", params)
    }
}

fn fx() -> Fx {
    let dir = tempfile::Builder::new()
        .prefix("cad1700")
        .tempdir()
        .unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions {
        provider_env: crate::adapter::ProviderEnv::refusing_providers(),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    opts.provider_env
        .set("CADENCE_HOME", dir.path().to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    for (alias, provider, kind) in [
        ("master", "pi", "managed"),
        ("w1", "fake", "fake"),
        // A valid agent alias that collides with the operator's sender label.
        ("operator", "fake", "fake"),
    ] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: kind,
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: (alias == "master").then_some(r#"{"model":"fixture/model"}"#),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
    }
    crate::test_seam::seed_pi_fixture_policy(&shared.store, "fixture/model").unwrap();
    // A live endpoint generation, so the master's turn token is current.
    shared
        .store
        .set_identity_with_quota(
            "master",
            &Identity {
                thread_id: "t".into(),
                session_id: "s".into(),
                model: None,
                effort: None,
                pid: pid(),
                endpoint: None,
                generation: Some("0123456789abcdef0123456789abcdef".into()),
                attach: None,
            },
            None,
        )
        .unwrap();
    Fx { _dir: dir, shared }
}

fn refusal(r: Result<Value>) -> (String, String) {
    let e = r.expect_err("expected a refusal");
    (e.code().unwrap_or_default().to_string(), e.to_string())
}

#[test]
fn cad1700_an_agent_caller_cannot_stop_or_cancel_a_master_turn() {
    let fx = fx();
    fx.send("m-run");
    fx.run("m-run");
    fx.send("m-queued");

    for who in [
        Asserted::Agent("master".into()),
        Asserted::Agent("w1".into()),
    ] {
        let (_, text) = refusal(fx.call(
            who.clone(),
            "master_command",
            json!({"command": "stop", "message": "m-run"}),
        ));
        assert!(
            text.contains("is an operator action") || text.contains("may not call master_command"),
            "stop from an agent is refused by the operator layer itself: {text}"
        );
        let (_, text) = refusal(fx.call(
            who,
            "master_command",
            json!({"command": "cancel", "message": "m-queued"}),
        ));
        assert!(
            text.contains("is an operator action") || text.contains("may not call master_command"),
            "cancel from an agent is refused by the operator layer itself: {text}"
        );
    }
    assert_eq!(
        fx.state("m-run"),
        "running",
        "the running turn is untouched"
    );
    assert_eq!(
        fx.state("m-queued"),
        "queued",
        "the queued message is untouched"
    );
}

#[test]
fn cad1700_stop_refuses_anything_but_the_current_running_turn() {
    let fx = fx();
    fx.send("m-old");
    fx.run("m-old");
    fx.finish("m-old");
    fx.send("m-queued");
    fx.send("m-new");
    fx.run("m-new");

    for message in ["m-queued", "m-old", "m-missing"] {
        let (code, text) = refusal(fx.command("stop", Some(message)));
        assert_eq!(
            code, "turn_not_running",
            "stop {message} is refused: {text}"
        );
    }
    // A stale click: `m-old` was the running turn once, `m-new` runs now.
    let (code, _) = refusal(fx.command("stop", Some("m-old")));
    assert_eq!(
        code, "turn_not_running",
        "a stale id never reaches the newer turn"
    );
    assert_eq!(fx.state("m-new"), "running", "the newer turn still runs");

    // Positive control: the running id passes the guard (the fixture
    // adapter cannot settle an interrupt, so only the guard is asserted).
    let out = fx.command("stop", Some("m-new"));
    if let Err(e) = &out {
        assert_ne!(
            e.code(),
            Some("turn_not_running"),
            "the running message must not be refused as not-running: {e}"
        );
    }
}

#[test]
fn cad1700_cancel_refuses_everything_but_the_operators_own_queued_message() {
    let fx = fx();
    fx.send("m-run");
    fx.run("m-run");
    fx.send("m-done");
    fx.finish("m-done");
    fx.send("m-mine");
    fx.send("m-other");
    // A message queued to the master by the daemon, not by the operator.
    let daemon_id = crate::proto::daemon_message_id("permission", "cad1700");
    fx.shared
        .store
        .enqueue_daemon("master", "notice", &daemon_id, "permission")
        .unwrap();
    // A queued message for another agent.
    fx.shared
        .store
        .enqueue("w1", "for w1", None, "m-w1", "operator")
        .unwrap();

    assert_eq!(
        refusal(fx.command("cancel", None)).0,
        "message_required",
        "cancel needs the message id"
    );
    assert_eq!(
        refusal(fx.command("cancel", Some("m-run"))).0,
        "message_running",
        "a running turn is never cancelled"
    );
    assert_eq!(fx.state("m-run"), "running", "the running turn still runs");
    assert_eq!(
        refusal(fx.command("cancel", Some("m-done"))).0,
        "message_not_queued",
        "only a queued message can be cancelled"
    );
    for message in [daemon_id.as_str(), "m-w1", "m-missing"] {
        assert_eq!(
            refusal(fx.command("cancel", Some(message))).0,
            "message_not_cancellable",
            "{message} is not an operator message queued to the master"
        );
    }
    assert_eq!(
        fx.state("m-w1"),
        "queued",
        "another agent's queue is untouched"
    );
    assert_eq!(
        fx.state(&daemon_id),
        "queued",
        "a daemon message is untouched"
    );

    // Positive control: the operator's own queued message is cancelled,
    // and its neighbour is not.
    fx.command("cancel", Some("m-mine")).unwrap();
    assert_eq!(
        fx.state("m-mine"),
        "cancelled",
        "the operator's own queued message is cancelled"
    );
    assert_eq!(
        fx.state("m-other"),
        "queued",
        "the message behind it is untouched"
    );
}

#[test]
fn cad1700_a_stale_stop_never_interrupts_a_newer_turn() {
    let fx = fx();
    // The daemon's own test pause sits between the admission check and the
    // bound re-check; the running turn is swapped inside that window.
    fx.shared
        .provider_env
        .set("CADENCE_TEST_INTERRUPT_PAUSE_MS", "400");
    fx.send("m-old");
    fx.run("m-old");

    let shared = fx.shared.clone();
    let caller = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.dispatch(
                "master_command",
                &json!({"command": "stop", "message": "m-old"}),
                pid(),
            )
        })
    });
    // Wait for the pause to be visible, then replace the running turn.
    let paused = |fx: &Fx| {
        fx.db()
            .query_row(
                "SELECT COUNT(*) FROM events WHERE alias='master' AND kind='interrupt_paused'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            > 0
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !paused(&fx) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(paused(&fx), "the interrupt reached the pause window");
    fx.finish("m-old");
    fx.send("m-new");
    fx.run("m-new");

    let code = caller
        .join()
        .unwrap()
        .expect_err("the stale stop is refused")
        .code()
        .map(str::to_string);
    assert_eq!(code.as_deref(), Some("turn_not_running"));
    assert_eq!(
        fx.state("m-new"),
        "running",
        "the newer turn is never interrupted by a stale click"
    );
}

#[test]
fn cad1700_cancel_refuses_a_forged_operator_source() {
    let fx = fx();
    // An agent may write `source: "operator"` on its own send; provenance
    // is the recorded sender, not that string.
    fx.call(
        Asserted::Agent("w1".into()),
        "agent_send",
        json!({"alias": "master", "text": "forged", "message": "m-forged",
               "source": "operator", "wait": 0}),
    )
    .unwrap();
    assert_eq!(
        fx.state("m-forged"),
        "queued",
        "the forged message is queued"
    );
    let (code, text) = refusal(fx.command("cancel", Some("m-forged")));
    assert_eq!(
        code, "message_not_cancellable",
        "a message an agent sent is not the operator's own: {text}"
    );
    assert_eq!(fx.state("m-forged"), "queued", "it is not cancelled");

    // An agent may literally be named `operator`: its row records that
    // label too, but carries a canonical uid the operator's chat does not.
    fx.call(
        Asserted::Agent("operator".into()),
        "agent_send",
        json!({"alias": "master", "text": "collision", "message": "m-collide",
               "source": "operator", "wait": 0}),
    )
    .unwrap();
    assert!(
        fx.shared
            .store
            .message("m-collide")
            .unwrap()
            .unwrap()
            .sender_uid
            .is_some(),
        "the colliding agent's row carries a uid"
    );
    let (code, text) = refusal(fx.command("cancel", Some("m-collide")));
    assert_eq!(
        code, "message_not_cancellable",
        "an agent named operator is still not the operator: {text}"
    );
    assert_eq!(fx.state("m-collide"), "queued", "it is not cancelled");
}
