//! CAD-1142 independent bad-case acceptance, authored by qa-sol-1142-sec2.
//!
//! The real managed Pi adapter binds a legitimate app_run_dispatch turn.
//! Its correlated app prompt response then refuses with success=false and
//! synthetic credential-bearing provider prose. The real actor must fail
//! the message/run and enter attention without publishing that prose to
//! Unproven or Agent readers. No failure, event or agent error is seeded.
//!
//! Existing ProviderEnv and caller seams only; no live model, spending,
//! production state, or operator raw-diagnostic read is involved (CAD-1243).
#![cfg(feature = "test-seam")]

use cadence_agent::adapter::{Identity, ProviderEnv};
use cadence_agent::store::{NewAgent, Store};
use cadence_agent::test_seam::{scoped, Asserted, Seam};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SENTINEL: &str = "qa1142_SYNTHETIC_SECRET_NOT_REAL";
const PROVIDER_PROSE: &str = "qa1142 synthetic provider prompt refused";
// Allow bounded Cadence recovery guidance without pinning its wording.
const DIAGNOSTIC_CHARS_MAX: usize = 1024;
const MANIFEST: &str = r#"---
app: fatal-reader
title: Fatal Reader
version: '1.0.0'
summary: Retain local text.
needs:
  connections: []
---

Retain local text.
"#;
const WORKFLOW: &str = r#"---
title: Local text
goal: Retain text
---
## Write
agent: writer
action: local.text.produce

Write Markdown.

### Acceptance
- [ ] Markdown exists
"#;

// This fixture models transport, not privacy. Responses echo the native
// request id and command, and bind/abandon receipts use the generated guard.
// Only the app prompt fails; control commands succeed normally.
const RUNTIME: &str = r#"
import json, pathlib, sys
args = sys.argv[1:]
model = args[args.index('--model') + 1]
provider, model_id = model.split('/', 1)
guard = str(pathlib.Path(args[args.index('--extension') + 1]).resolve())
error = 'qa1142 synthetic provider prompt refused; Authorization: Bearer qa1142_SYNTHETIC_SECRET_NOT_REAL'

def emit(value):
    print(json.dumps(value), flush=True)

def journal(value):
    with open('runtime.jsonl', 'a') as file:
        file.write(json.dumps(value) + '\n')

for line in sys.stdin:
    req = json.loads(line)
    kind = req.get('type')
    reply = {'type':'response', 'id':req.get('id'), 'command':kind, 'success':True}
    if kind == 'get_commands':
        reply['data'] = {'commands':[
            {'name':name, 'source':'extension', 'sourceInfo':{'path':guard}}
            for name in ['cadence-bind-turn', 'cadence-steer-turn', 'cadence-abandon-turn']]}
    elif kind == 'get_state':
        reply['data'] = {'sessionId':'synthetic-session', 'model':{'provider':provider, 'id':model_id}}
    elif kind == 'prompt' and req.get('message', '').startswith('/cadence-bind-turn '):
        data = json.loads(req['message'].split(' ', 1)[1])
        data.update({'version':1, 'operation':'bind', 'outcome':'bound'})
        emit({'type':'entry_appended', 'entry':{'customType':'cadence-turn-input', 'data':data}})
        journal({'rpc':'bind', 'turn':data['turn']})
    elif kind == 'prompt' and req.get('message', '').startswith('/cadence-abandon-turn '):
        data = json.loads(req['message'].split(' ', 1)[1])
        data.update({'version':1, 'operation':'abandon', 'outcome':'abandoned'})
        emit({'type':'entry_appended', 'entry':{'customType':'cadence-turn-input', 'data':data}})
    elif kind == 'prompt':
        reply.update({'success':False, 'error':error})
        journal({'rpc':'app_prompt', 'app_owned': '"run_id"' in req.get('message', ''),
                 'response':reply})
    emit(reply)
"#;

struct Fx {
    root: tempfile::TempDir,
    store: Store,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
}

impl Drop for Fx {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .expect("daemon thread panicked")
                .expect("daemon failed");
        }
    }
}

impl Fx {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1142fatal")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let yaml = pm.join("pm.yaml");
        let mut config = std::fs::read_to_string(&yaml).unwrap();
        // Merely reflected by the stdio fixture: no real provider/model runs.
        config.push_str("\npi:\n  models:\n    allow: [\"fake/model-1\"]\n");
        std::fs::write(yaml, config).unwrap();
        let script = root.path().join("runtime.py");
        std::fs::write(&script, RUNTIME).unwrap();
        let source = root.path().join("app");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("app.md"), MANIFEST).unwrap();
        std::fs::write(source.join("workflows/write.md"), WORKFLOW).unwrap();
        let env = ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        env.set(
            "CADENCE_PI_COMMAND",
            format!("python3 {}", script.display()),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let options = daemon::ServeOptions {
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
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let thread = std::thread::spawn(move || daemon::serve_with(&state, options));
        let fx = Self {
            root,
            store,
            stop,
            thread: Some(thread),
        };
        fx.until("armed daemon", || {
            client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(1)).is_ok()
                && Seam::token_at(&fx.state()).is_some()
        });
        fx.register_team();
        fx.op("agent_resume", json!({"alias":"writer"}));
        fx.until(
            "actual Pi endpoint ready before freezing assignment",
            || {
                let writer = fx.store().agent("writer").unwrap();
                assert_ne!(
                    writer.state, "attention",
                    "Pi fixture failed to open: {:?}",
                    writer.error
                );
                writer.enabled
                    && writer.state == "idle"
                    && writer.session_id.is_some()
                    && fx.op("agent_show", json!({"alias":"writer"}))["agent"]
                        ["native_turn_steering_enabled"]
                        == true
            },
        );
        fx
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("s")
    }
    fn store(&self) -> &Store {
        // Opened before actors start: Store::open performs crash recovery,
        // so reopening it as an observer would reset live endpoint state.
        &self.store
    }
    fn rpc(&self, who: Asserted, method: &str, params: Value) -> Value {
        scoped(who, || client::rpc(&self.state(), method, params))
            .unwrap_or_else(|e| panic!("{method}: {e}"))
    }
    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
    }
    fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done() {
            assert!(
                Instant::now() < deadline,
                "{what}: timed out; runtime: {:?}; worker: {:?}; stderr: {}",
                self.journal(),
                self.store().agent("writer").ok(),
                std::fs::read_to_string(self.state().join("agents/writer.provider.log"))
                    .unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn journal(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.path().join("runtime.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn register_team(&self) {
        let store = self.store();
        let cwd = self.root.path().to_str().unwrap();
        for (alias, role, provider, params) in [
            ("lead", "pm", "claude", None),
            (
                "writer",
                "worker",
                "pi",
                Some(r#"{"upstream":"lead","model":"fake/model-1"}"#),
            ),
        ] {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider,
                    endpoint_kind: "managed",
                    role,
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        store
            .set_identity(
                "lead",
                &Identity {
                    thread_id: "lead-thread".into(),
                    session_id: "lead-session".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some("lead-generation".into()),
                    attach: None,
                },
            )
            .unwrap();
        // Writer identity is supplied by the actual Pi adapter at resume,
        // before the run freezes its assignment. Never forge its turn.
    }
    fn dispatch(&self) -> (String, String) {
        let installed = self.op(
            "app_workspace_install",
            json!({"source":self.root.path().join("app")}),
        );
        let run = self.op(
            "app_run_create",
            json!({
                "install_id":installed["install_id"], "workflow":"write", "inputs":{},
                "request_id":"actor-fatal-privacy", "owner_pm":"lead"
            }),
        );
        let id = run["id"].as_str().unwrap().to_owned();
        self.op(
            "app_run_approve",
            json!({"run_id":id,"digest":run["snapshot_digest"]}),
        );
        let dispatched = self.op("app_run_dispatch", json!({"run_id":id}));
        let message = dispatched["steps"][0]["message_id"]
            .as_str()
            .unwrap()
            .to_owned();
        (id, message)
    }
}

fn diagnostic(value: &Value, label: &str) {
    let reason = value
        .as_str()
        .unwrap_or_else(|| panic!("{label}: no diagnostic string"));
    assert!(
        !reason.trim().is_empty() && reason.chars().any(char::is_alphabetic),
        "{label}: no human-readable diagnostic"
    );
    assert!(
        reason.chars().count() <= DIAGNOSTIC_CHARS_MAX,
        "{label}: unbounded diagnostic"
    );
}

#[test]
fn app_actor_fatal_error_keeps_private_provider_detail_off_public_reads() {
    let fx = Fx::start();
    let (run_id, message_id) = fx.dispatch();
    fx.until("actual failed app prompt and public attention", || {
        let store = fx.store();
        store
            .message(&message_id)
            .unwrap()
            .is_some_and(|m| m.state == "failed")
            && store.agent("writer").unwrap().state == "attention"
            && store
                .events("writer", 0, 100)
                .unwrap()
                .iter()
                .any(|e| e.kind == "attention")
    });
    let journal = fx.journal();
    assert!(
        journal
            .iter()
            .any(|r| r["rpc"] == "bind" && r["turn"].as_str().is_some_and(|s| !s.is_empty())),
        "native turn never bound"
    );
    let refused = journal
        .iter()
        .find(|r| r["rpc"] == "app_prompt" && r["app_owned"] == true)
        .expect("no app-owned runtime prompt");
    assert_eq!(refused["response"]["command"], "prompt");
    assert_eq!(refused["response"]["success"], false);
    assert!(
        refused["response"]["id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "prompt refusal was not correlated"
    );
    let raw = refused["response"]["error"].as_str().unwrap();
    assert!(raw.contains(PROVIDER_PROSE) && raw.contains(SENTINEL));
    let message = fx.store().message(&message_id).unwrap().unwrap();
    assert_eq!(message.source, "app_run_dispatch");
    assert_eq!(message.state, "failed", "message did not actually fail");
    assert_eq!(message.result.as_ref().unwrap()["status"], "failed");
    assert!(
        message
            .error
            .as_deref()
            .is_some_and(|s| s.contains(SENTINEL) && s.contains(PROVIDER_PROSE)),
        "failed message did not originate from the synthetic prompt refusal"
    );
    let shown = fx.op("app_run_show", json!({"run_id":run_id}));
    assert_eq!(shown["state"], "failed", "run did not actually fail");
    assert_eq!(shown["steps"][0]["state"], "failed", "step did not fail");
    diagnostic(&shown["steps"][0]["reason"], "failed step");

    // Obtain positive witnesses on every public read for both callers before
    // reporting disclosures. A missing event/worker/reason cannot pass.
    let mut disclosures = Vec::new();
    for who in [Asserted::Unproven, Asserted::Agent("lead".into())] {
        let label = format!("{who:?}");
        let events = fx.rpc(
            who.clone(),
            "agent_events",
            json!({"alias":"writer","after":0}),
        );
        let attention = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == "attention")
            .expect("no actual public attention event");
        diagnostic(&attention["payload"]["reason"], "attention event");
        let show = fx.rpc(who.clone(), "agent_show", json!({"alias":"writer"}));
        assert_eq!(show["agent"]["state"], "attention", "{show}");
        diagnostic(&show["agent"]["error"], "agent_show attention");
        let list = fx.rpc(who.clone(), "agent_list", json!({}));
        let writer = list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alias"] == "writer")
            .expect("failed worker absent from agent_list");
        assert_eq!(writer["state"], "attention");
        diagnostic(&writer["error"], "agent_list attention");
        let wait = fx.rpc(
            who,
            "agent_wait",
            json!({"alias":"writer","until":"attention","timeout":1}),
        );
        assert_eq!(wait["state"], "attention");
        diagnostic(&wait["reason"], "agent_wait attention");
        for (surface, value) in [
            ("agent_events", &events),
            ("agent_show", &show),
            ("agent_list", &list),
            ("agent_wait", &wait),
        ] {
            let text = value.to_string();
            if text.contains(SENTINEL) || text.contains(PROVIDER_PROSE) {
                disclosures.push(format!("{label} {surface} exposed private provider prose"));
            }
        }
    }
    if shown.to_string().contains(SENTINEL) || shown.to_string().contains(PROVIDER_PROSE) {
        disclosures.push("app_run_show exposed private provider prose".into());
    }
    assert!(
        disclosures.is_empty(),
        "actor-fatal app privacy violated: {}",
        disclosures.join("; ")
    );
}
