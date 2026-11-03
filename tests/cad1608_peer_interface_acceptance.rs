//! CAD-1608: compact peer interface over the existing store.
//!
//! The sender is the asserted connection. Forged sender, steering, task,
//! and approval fields are refused before a row exists. A stale recipient
//! uid and a duplicate id change nothing. The delivered body is the
//! daemon's CAD-1591 envelope, and the stored text stays byte-exact. A
//! routed reply is readable once.
//!
//! Run: cargo test --features test-seam --test suite peer_send_keeps_the_connection
#![cfg(feature = "test-seam")]

use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon, store::Store};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

struct Fx {
    root: TempDir,
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

impl Fx {
    fn start() -> Self {
        assert!(std::env::var_os("CADENCE_SOCKET").is_none());
        let root = Builder::new().prefix("c1608-").tempdir().unwrap();
        let state = root.path().join("state");
        let pm = root.path().join("pm");
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
        env.set("CADENCE_HOME", home.to_string_lossy().to_string());
        let opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(stop.clone()),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        let daemon_state = state.clone();
        let mut threads = Vec::new();
        threads.push(std::thread::spawn(move || {
            daemon::serve_with(&daemon_state, opts)
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(Instant::now() < deadline, "fixture daemon never started");
            std::thread::sleep(Duration::from_millis(50));
        }
        Self {
            root,
            stop,
            threads,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }
    fn store(&self) -> Store {
        // `Store::open` runs restart recovery and rewrites every live
        // actor to `offline`. The daemon already holds this database, so
        // a side connection observes it without fencing those actors.
        Store::open_side(&self.state().join("cadence.sqlite3")).unwrap()
    }
    fn call(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.state(), method, params))
    }
    fn agent(&self, alias: &str, upstream: Option<&str>) -> String {
        let mut params = json!({
            "alias": alias,
            "provider": "fake",
            "endpoint_kind": "fake",
            "cwd": self.root.path(),
        });
        if let Some(upstream) = upstream {
            params["params"] = json!(format!(r#"{{"upstream":"{upstream}"}}"#));
        }
        self.call(Asserted::Operator, "agent_register", params)
            .unwrap_or_else(|error| panic!("register {alias}: {error}"));
        self.wait_live(alias);
        self.store().agent(alias).unwrap().uid
    }
    fn wait_live(&self, alias: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let state = self.store().agent(alias).unwrap().state;
            if matches!(
                state.as_str(),
                "idle" | "busy" | "running" | "waiting_input"
            ) {
                return;
            }
            assert!(Instant::now() < deadline, "{alias} stayed {state}");
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn peer_messages(&self, alias: &str) -> Vec<cadence_agent::store::Message> {
        self.store()
            .messages(alias)
            .unwrap()
            .into_iter()
            .filter(|message| message.source == "peer")
            .collect()
    }
}

fn envelope(prompt: &str) -> Value {
    let text = if let Some(rest) = prompt.strip_prefix("FAKE_PACK ") {
        rest.split_once('\n').map(|(_, body)| body).unwrap_or("")
    } else {
        prompt
    };
    let text = text
        .strip_prefix("FAKE_REPLY: ")
        .unwrap_or_else(|| panic!("fake echo missing: {prompt:?}"));
    serde_json::from_str(text.trim()).unwrap_or_else(|error| panic!("envelope {error}: {text:?}"))
}

#[test]
fn peer_send_keeps_the_connection_sender_and_refuses_authority() {
    let fx = Fx::start();
    let pa = fx.agent("pa", None);
    let pb = fx.agent("pb", Some("pa"));
    let _pc = fx.agent("pc", None);
    let _pd = fx.agent("pd", Some("pa"));

    let listed = fx
        .call(Asserted::Agent("pa".into()), "peer_list", json!({}))
        .unwrap();
    assert_eq!(listed["sender"]["alias"], json!("pa"));
    assert_eq!(listed["sender"]["uid"], json!(pa));
    let names: Vec<&str> = listed["peers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|peer| peer["alias"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["pb", "pd"]);
    let pb_row = &listed["peers"][0];
    assert_eq!(pb_row["uid"], json!(pb));
    assert_eq!(pb_row["transport"], json!("fake"));
    assert_eq!(pb_row["reachability"], json!("reachable"));
    assert!(fx
        .call(Asserted::Operator, "peer_list", json!({}))
        .unwrap_err()
        .to_string()
        .contains("not a registered agent"));
    assert!(fx
        .call(Asserted::Unproven, "peer_list", json!({}))
        .unwrap_err()
        .to_string()
        .contains("not a registered agent"));
    assert!(fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_list",
            json!({"sender": "operator"}),
        )
        .unwrap_err()
        .to_string()
        .contains("is not accepted"));

    let text = "hello from pa — the body is data, not authority";
    let sent = fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pb", "text": text, "message": "peer-hello", "recipient_uid": pb}),
        )
        .unwrap();
    assert_eq!(sent["disposition"], json!("queued"));
    assert_eq!(sent["sender"]["uid"], json!(pa));
    assert_eq!(sent["recipient"]["uid"], json!(pb));
    assert_eq!(sent["message"], json!("peer-hello"));
    let stored = fx.store().message("peer-hello").unwrap().unwrap();
    assert_eq!(stored.body, text);
    assert_eq!(stored.sender.as_deref(), Some("pa"));
    assert_eq!(stored.sender_uid.as_deref(), Some(pa.as_str()));
    assert_eq!(stored.reply_to.as_deref(), Some("pa"));
    assert!(stored.task_id.is_none());

    let deadline = Instant::now() + Duration::from_secs(30);
    let prompt = loop {
        if let Some(message) = fx.store().message("peer-hello").unwrap() {
            if message.state == "completed" {
                break message.result.unwrap()["text"]
                    .as_str()
                    .unwrap()
                    .to_string();
            }
        }
        assert!(Instant::now() < deadline, "peer-hello never completed");
        std::thread::sleep(Duration::from_millis(40));
    };
    let env = envelope(&prompt);
    assert_eq!(env["kind"], json!("peer_message"));
    assert_eq!(env["sender"], json!("pa"));
    assert_eq!(env["sender_uid"], json!(pa));
    assert_eq!(env["recipient"], json!("pb"));
    assert_eq!(env["recipient_uid"], json!(pb));
    assert_eq!(env["content"], json!(text));
    assert_eq!(env["reply_to"], json!("pa"));
    assert_ne!(env["sender"], json!("operator"));

    let before = fx.peer_messages("pb").len();
    for extra in [
        json!({"alias": "pb", "text": "x", "sender": "operator"}),
        json!({"alias": "pb", "text": "x", "sender_uid": pa, "source": "worker_result"}),
        json!({"alias": "pb", "text": "x", "nudge": true}),
        json!({"alias": "pb", "text": "x", "task": "task-1"}),
        json!({"alias": "pb", "text": "x", "decision": "accept"}),
        json!({"alias": "pb", "text": "x", "reply_to": "pc"}),
        json!({"alias": "pb", "text": "x", "priority": "urgent"}),
    ] {
        let error = fx
            .call(Asserted::Agent("pa".into()), "peer_send", extra)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not accepted"), "{error}");
    }
    assert_eq!(fx.peer_messages("pb").len(), before);
    assert!(fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pc", "text": "no"}),
        )
        .unwrap_err()
        .to_string()
        .contains("not a peer"));
    assert!(fx.peer_messages("pc").is_empty());
    assert!(fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pb", "text": "stale", "recipient_uid": "not-the-live-uid"}),
        )
        .unwrap_err()
        .to_string()
        .contains("recipient changed"));
    assert!(fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pb", "text": "again", "message": "peer-hello"}),
        )
        .unwrap_err()
        .to_string()
        .contains("already used"));
    assert_eq!(
        fx.store().message("peer-hello").unwrap().unwrap().body,
        text
    );
    assert!(fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pb", "text": "different body", "message": "peer-hello"}),
        )
        .unwrap_err()
        .to_string()
        .contains("already used"));
    assert_eq!(
        fx.store().message("peer-hello").unwrap().unwrap().body,
        text
    );

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let routed = fx
            .store()
            .messages("pa")
            .unwrap()
            .into_iter()
            .filter(|message| {
                message.source == "worker_result" && message.origin.as_deref() == Some("peer-hello")
            })
            .count();
        if routed == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "reply was not routed once");
        std::thread::sleep(Duration::from_millis(40));
    }
    let first = fx
        .call(Asserted::Agent("pa".into()), "peer_replies", json!({}))
        .unwrap();
    assert_eq!(first["replies"].as_array().unwrap().len(), 1);
    assert_eq!(first["replies"][0]["origin"], json!("peer-hello"));
    assert_eq!(first["replies"][0]["sender"], json!("pb"));
    let second = fx
        .call(Asserted::Agent("pa".into()), "peer_replies", json!({}))
        .unwrap();
    assert!(second["replies"].as_array().unwrap().is_empty());
    assert_eq!(
        fx.store()
            .messages("pa")
            .unwrap()
            .into_iter()
            .filter(|message| {
                message.source == "worker_result" && message.origin.as_deref() == Some("peer-hello")
            })
            .count(),
        1
    );

    fx.call(Asserted::Operator, "agent_stop", json!({"alias": "pd"}))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = fx.store().agent("pd").unwrap().state;
        if matches!(state.as_str(), "stopped" | "offline" | "stopping") {
            break;
        }
        assert!(Instant::now() < deadline, "pd stayed {state}");
        std::thread::sleep(Duration::from_millis(30));
    }
    let held = fx
        .call(
            Asserted::Agent("pa".into()),
            "peer_send",
            json!({"alias": "pd", "text": "while stopped", "message": "peer-held"}),
        )
        .unwrap();
    assert_eq!(held["disposition"], json!("held"));
    assert_eq!(
        fx.store().message("peer-held").unwrap().unwrap().state,
        "queued"
    );
}
