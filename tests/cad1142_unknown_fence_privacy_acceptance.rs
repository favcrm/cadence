//! CAD-1142 independent acceptance, authored by qa-sol-1142-sec.
//!
//! A real managed Pi adapter talks to an isolated stdio runtime fixture.
//! A correlated clear_queue error after an app-owned turn settles reaches
//! the real actor's OutcomeUnknown -> Shared::unknown path. No direct event,
//! agent error, terminal message or fence is seeded by the test.
//!
//! Public readers must see that uncertainty really fenced the worker, but
//! never the synthetic provider prose/secret. Existing ProviderEnv and caller
//! seams suffice; no model call or production process is involved.
//!
//! cloud_hold is not a reachable app-worker branch here: LocalRunRequest's
//! actual team guard admits managed local workers (and fake), explicitly
//! excluding devin/cloud. Forging an app association or relaxing that guard
//! would not be acceptance of a legitimate app turn, so this file does not.
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
const PROVIDER_PROSE: &str = "qa1142 provider cleanup refused";
// Public fences include Cadence's recovery guidance, not just the class.
const DIAGNOSTIC_CHARS_MAX: usize = 1024;
const MANIFEST: &str = r#"---
app: unknown-reader
title: Unknown Reader
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

// This models the Pi stdio protocol, not the privacy policy. The real
// adapter discovers the real generated guard path and validates its
// correlated bind receipt; only post-settlement cleanup is made to fail.
const RUNTIME: &str = r#"
import json, pathlib, sys
args = sys.argv[1:]
model = args[args.index('--model') + 1]
provider, model_id = model.split('/', 1)
guard = str(pathlib.Path(args[args.index('--extension') + 1]).resolve())
error = 'qa1142 provider cleanup refused; Authorization: Bearer qa1142_SYNTHETIC_SECRET_NOT_REAL'

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
    elif kind == 'prompt':
        journal({'rpc':'app_prompt', 'app_owned': '"run_id"' in req.get('message', '')})
        emit(reply)
        emit({'type':'message_end', 'message':{'role':'assistant', 'content':[], 'stopReason':'stop'}})
        emit({'type':'agent_settled'})
        continue
    elif kind == 'clear_queue':
        journal({'rpc':'clear_queue', 'success':False, 'error':error})
        reply.update({'success':False, 'error':error})
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
            .prefix("c1142unk")
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
                "request_id":"unknown-privacy", "owner_pm":"lead"
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
    assert!(!reason.trim().is_empty(), "{label}: empty diagnostic");
    assert!(
        reason.chars().count() <= DIAGNOSTIC_CHARS_MAX,
        "{label}: unbounded diagnostic"
    );
}

#[test]
fn app_unknown_fence_keeps_private_provider_detail_off_public_reads() {
    let fx = Fx::start();
    let (run_id, message_id) = fx.dispatch();
    fx.until("actual uncertain app turn and public attention", || {
        let store = fx.store();
        store
            .message(&message_id)
            .unwrap()
            .is_some_and(|m| m.state == "unknown")
            && store.agent("writer").unwrap().state == "attention"
            && store
                .events("writer", 0, 100)
                .unwrap()
                .iter()
                .any(|e| e.kind == "attention")
    });
    let journal = fx.journal();
    assert!(
        journal.iter().any(|r| r["rpc"] == "bind"),
        "native turn never bound"
    );
    assert!(
        journal
            .iter()
            .any(|r| r["rpc"] == "app_prompt" && r["app_owned"] == true),
        "no app-owned runtime turn"
    );
    assert!(
        journal.iter().any(|r| r["rpc"] == "clear_queue"
            && r["success"] == false
            && r["error"].as_str().is_some_and(|s| s.contains(SENTINEL))),
        "uncertain cleanup failure never happened"
    );
    let message = fx.store().message(&message_id).unwrap().unwrap();
    assert_eq!(message.source, "app_run_dispatch");
    assert_eq!(message.state, "unknown");
    assert!(message.started.is_some(), "unknown turn was never started");
    let shown = fx.op("app_run_show", json!({"run_id":run_id}));
    assert_eq!(shown["state"], "failed");
    assert_eq!(shown["steps"][0]["state"], "failed");
    diagnostic(&shown["steps"][0]["reason"], "failed step");

    // Collect every caller/surface before failing, so the red execution
    // positively observes both reader classes and all read projections.
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
            .unwrap();
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
    let step = shown["steps"][0]["reason"].as_str().unwrap();
    if step.contains(SENTINEL) || step.contains(PROVIDER_PROSE) {
        disclosures.push("steps[].reason exposed private provider prose".into());
    }
    assert!(
        disclosures.is_empty(),
        "unknown-fence privacy violated: {}",
        disclosures.join("; ")
    );
}
