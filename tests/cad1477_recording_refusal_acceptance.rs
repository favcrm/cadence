//! CAD-1477 independently authored recording refusal acceptance.
//! The implementation owner must not edit or weaken this check.
//! Exercises the real native RPC and board guards in an isolated fixture.
//! No provider response, clock, operator guard or recording service is mocked.
#![cfg(feature = "test-seam")]

use base64::Engine;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon, operator_auth};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

type Session = (String, String);

struct Fixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    board: Option<(Arc<AtomicBool>, std::thread::JoinHandle<()>)>,
    port: u16,
}

impl Fixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cad1477refusal")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        cadence_agent::issue::Pm::init(&pm).unwrap();
        let state = root.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let provider_env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env,
            stop: Some(Arc::clone(&stop)),
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
        let daemon = std::thread::spawn(move || daemon::serve_with(&daemon_state, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&state).is_none()
        {
            assert!(
                !daemon.is_finished() && std::time::Instant::now() < deadline,
                "isolated recording daemon did not become ready"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let port = (3110..=3199)
            .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
            .expect("free isolated board port in 3110-3199");
        let (tx, rx) = std::sync::mpsc::channel();
        let board_stop = Arc::new(AtomicBool::new(false));
        let board_opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&board_stop)),
            startup: Some(tx),
            test_seam: true,
            ..Default::default()
        };
        let board = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&state, &pm, &board_opts);
        });
        rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
        Self {
            root,
            stop,
            daemon: Some(daemon),
            board: Some((board_stop, board)),
            port,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.path().join("state")
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || client::rpc(&self.state(), method, params))
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("operator {method}: {error}"))
    }

    fn http(
        &self,
        who: &Asserted,
        method: &str,
        path: &str,
        body: &Value,
        session: Option<&Session>,
    ) -> (u16, Value, Option<String>) {
        let host = format!("cadence-{}.localhost:{}", self.port, self.port);
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let mut request = match method {
            "POST" => agent.post(&url),
            "PATCH" => agent.patch(&url),
            other => panic!("unsupported fixture method {other}"),
        }
        .header("Host", &host)
        .header("Origin", format!("http://{host}"))
        .header("Sec-Fetch-Site", "same-origin")
        .header("X-Cadence-Board", "1")
        .header("Content-Type", "application/json")
        .header(AS_HEADER, who.as_str())
        .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
        if let Some((cookie, key)) = session {
            request = request
                .header("Cookie", cookie)
                .header("X-Cadence-Session", key);
        }
        let mut response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.split(';').next().unwrap().to_owned());
        let text = response.body_mut().read_to_string().unwrap();
        let value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{method} {path}: {status}: {text}: {error}"));
        (status, value, cookie)
    }

    fn session(&self) -> Session {
        operator_auth::ensure_secret(&self.state()).unwrap();
        let secret = operator_auth::read_secret(&self.state()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let (status, reply, cookie) = self.http(
            &Asserted::Operator,
            "POST",
            "/api/session",
            &json!({"nonce": nonce}),
            None,
        );
        assert_eq!(status, 200, "real operator session exchange: {reply}");
        (
            cookie.expect("operator session cookie"),
            reply["session_key"]
                .as_str()
                .expect("operator session key")
                .to_owned(),
        )
    }

    fn install_and_context(&self) -> (String, String) {
        let source = self.root.path().join("recording-app");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            r#"---
app: recording-refusal
title: Recording refusal fixture
version: '0.1.0'
summary: Isolated native recording authorization fixture.
needs:
  connections: []
  capabilities:
    speech:
      schema: 1
      capability: audio.transcribe
      version: 1
      action: transcribe_audio
      resource_kind: connection_account
      effect: draft
    minutes:
      schema: 1
      capability: text.generate
      version: 1
      action: generate_text
      resource_kind: connection_account
      effect: draft
  recording:
    transcription: speech
    consolidation: minutes
---

# Recording refusal fixture
"#,
        )
        .unwrap();
        // Satisfy the shared package validator; never dispatch this workflow.
        std::fs::write(
            source.join("workflows/note.md"),
            r#"---
title: Note
goal: One local note
inputs:
  writer: { ask: "writer" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one local note.

### Acceptance
- [ ] note exists
"#,
        )
        .unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        assert_eq!(
            installed["approved"], true,
            "fixture must be operator-approved: {installed}"
        );
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let created = self.op("app_context_create", json!({"install_id": install,
            "label": "Recording refusal", "input_defaults": {}, "request_id": "ctx-recording-refusal"}));
        let context = created["context"]["id"]
            .as_str()
            .or_else(|| created["id"].as_str())
            .unwrap()
            .to_owned();
        (install, context)
    }

    fn assert_no_recording_or_provider_work(&self, install: &str, label: &str) {
        let record_path =
            cadence_agent::store::app_records::record_db_path(&self.state(), install).unwrap();
        let records =
            Connection::open_with_flags(record_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        for table in [
            "app_recordings",
            "app_recording_segments",
            "app_recording_jobs",
            "app_recording_requests",
        ] {
            let count: i64 = records
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "{label}: refused request created recording/provider work in {table}"
            );
        }
        let core = Connection::open_with_flags(
            self.state().join("cadence.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        for table in [
            "app_bindings",
            "app_runs",
            "app_capability_claims",
            "app_capability_results",
        ] {
            let count: i64 = core
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(
                count, 0,
                "{label}: refused request created binding/run/provider work in {table}"
            );
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some((stop, board)) = self.board.take() {
            stop.store(true, SeqCst);
            let _ = board.join();
        }
        self.stop.store(true, SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.join();
        }
    }
}

fn upload_body() -> Value {
    // Complete canonical 10 ms, 16 kHz mono PCM16 WAV; auth refusal must
    // not accidentally be satisfied by a malformed audio request.
    let data_bytes = 320_u32;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&16_000_u32.to_le_bytes());
    wav.extend_from_slice(&32_000_u32.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());
    wav.resize(44 + data_bytes as usize, 0);
    json!({"sequence": 0, "digest": format!("{:x}", Sha256::digest(&wav)),
        "start_ms": 0, "end_ms": 10, "format": "wav16k-mono-pcm16",
        "audio_base64": base64::engine::general_purpose::STANDARD.encode(&wav)})
}

fn forge_authority(value: &mut Value) {
    let fields = value.as_object_mut().unwrap();
    fields.insert("operator".into(), json!(true));
    fields.insert("actor".into(), json!("operator"));
    fields.insert("as".into(), json!("operator"));
    fields.insert("pid".into(), json!(std::process::id()));
}

#[test]
fn non_operators_cannot_start_upload_finish_or_retry_recordings() {
    let fx = Fixture::start();
    let (install, context) = fx.install_and_context();
    let root = format!("/api/app-installations/{install}/contexts/{context}/recordings");
    let record = "rec_refusal_target";
    let listed = fx.op(
        "app_recording_list",
        json!({"install_id": install,
        "context_id": context, "body": {}}),
    );
    assert_eq!(
        listed["recordings"],
        json!([]),
        "operator scope/list control: {listed}"
    );
    let control_session = fx.session();
    let (status, quote, _) = fx.http(
        &Asserted::Operator,
        "POST",
        &format!("{root}/quote"),
        &json!({}),
        Some(&control_session),
    );
    assert_eq!(
        status, 200,
        "operator HTTP peer/session/scope control: {quote}"
    );
    assert!(
        quote["unavailable_reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "unfunded fixture must fail closed rather than invent provider availability: {quote}"
    );
    assert!(
        quote["transcription_max_minor"].is_null() && quote["consolidation_max_minor"].is_null()
    );
    fx.assert_no_recording_or_provider_work(&install, "operator controls");

    let operations = [
        (
            "app_recording_quote",
            "POST",
            format!("{root}/quote"),
            false,
            json!({}),
        ),
        (
            "app_recording_create",
            "POST",
            root.clone(),
            false,
            json!({"request_id": "refused-create", "title": "Unauthorized meeting",
                "participants": [], "consent": {"transcription_max_minor": 1_000_000,
                    "consolidation_max_minor": 1_000_000, "currency": "USD"}}),
        ),
        (
            "app_recording_segment",
            "POST",
            format!("{root}/{record}/segments"),
            true,
            upload_body(),
        ),
        (
            "app_recording_finish",
            "POST",
            format!("{root}/{record}/finish"),
            true,
            json!({"request_id": "refused-finish", "last_sequence": -1}),
        ),
        (
            "app_recording_retry",
            "POST",
            format!("{root}/{record}/retry"),
            true,
            json!({"request_id": "refused-retry"}),
        ),
        (
            "app_recording_rename",
            "PATCH",
            format!("{root}/{record}"),
            true,
            json!({"request_id": "refused-rename", "title": "Forged operator title"}),
        ),
    ];
    for who in [
        Asserted::Agent("recording-agent".into()),
        Asserted::Unproven,
    ] {
        for (rpc_method, http_method, path, has_record, body) in &operations {
            // Clean inputs prove the actual connection guard refuses; a
            // separate forged envelope proves identity fields cannot promote it.
            // A third case carries genuine operator credentials in the RPC body.
            for variant in 0..3 {
                let mut params =
                    json!({"install_id": install, "context_id": context, "body": body});
                if *has_record {
                    params["recording_id"] = json!(record);
                }
                if variant == 1 {
                    forge_authority(&mut params);
                }
                if variant == 2 {
                    let (cookie, key) = fx.session();
                    params["token"] = json!(cookie.split_once('=').unwrap().1);
                    params["key"] = json!(key);
                    params["origin"] = json!("loopback");
                    forge_authority(&mut params["body"]);
                }
                let label = format!("{who:?} {rpc_method} RPC variant {variant}");
                let error = fx
                    .rpc(who.clone(), rpc_method, params)
                    .expect_err(&label)
                    .to_string();
                let expected = if variant == 1 {
                    "app recording management authority is connection-bound; request field"
                } else {
                    "app recording management is an operator action"
                };
                assert!(
                    error.contains(expected),
                    "{label}: refused for the wrong reason: {error}"
                );
                fx.assert_no_recording_or_provider_work(&install, &label);
            }
            for forged in [false, true] {
                for carry_session in [false, true] {
                    // Agent presentation revokes a stolen session, so every
                    // attempt gets its own fresh, legitimately minted session.
                    let session = carry_session.then(|| fx.session());
                    let mut sent_body = body.clone();
                    if forged {
                        forge_authority(&mut sent_body);
                    }
                    let label = format!(
                        "{who:?} {http_method} {path} forged={forged} session={carry_session}"
                    );
                    let (status, reply, _) =
                        fx.http(&who, http_method, path, &sent_body, session.as_ref());
                    assert_eq!(status, 403, "{label}: {reply}");
                    let expected_check = match (&who, carry_session) {
                        (Asserted::Agent(_), false) => "operator_only",
                        (Asserted::Agent(_), true) => "session_from_agent",
                        (Asserted::Unproven, _) => "caller_identity",
                        _ => unreachable!(),
                    };
                    assert_eq!(
                        reply["check"].as_str(),
                        Some(expected_check),
                        "{label}: refused for the wrong reason: {reply}"
                    );
                    fx.assert_no_recording_or_provider_work(&install, &label);
                }
            }
        }
    }
    let after = fx.op(
        "app_recording_list",
        json!({"install_id": install,
        "context_id": context, "body": {}}),
    );
    assert_eq!(
        after["recordings"],
        json!([]),
        "refused requests became operator-visible recordings: {after}"
    );
    fx.assert_no_recording_or_provider_work(&install, "entire refusal matrix");
}
