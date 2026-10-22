//! CAD-1739 independently authored native app-lifecycle refusal acceptance.
//! Assertions use the real daemon RPC and loopback board HTTP, never workspace
//! functions, table counts or substituted native responses. Test-seam callers,
//! agents and grants are explicitly synthetic scratch fixtures, NOT production
//! authentication evidence. No provider is allowed to execute.

#[cfg(not(all(feature = "test-seam", target_os = "linux")))]
#[test]
fn hosted_app_mutations_preserve_native_guards() {
    panic!("CAD-1739 hosted app acceptance requires Linux and --features test-seam");
}

#[cfg(all(feature = "test-seam", target_os = "linux"))]
mod acceptance {
    use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
    use cadence_agent::{client, daemon, issue};
    use serde_json::{json, Value};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const TEST: &str = "acceptance::hosted_app_mutations_preserve_native_guards";
    const CHILD: &str = "CADENCE_CAD1739_APP_ACCEPT_CHILD";
    const WORKFLOW: &str = r#"---
title: "Draft: {{topic}}"
goal: "Retain a local text draft"
inputs:
  topic: { ask: "Draft topic" }
  writer: { ask: "Registered scratch writer" }
---

A local-only draft. No sending, tools or external effects.

## Draft {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Write one text artifact.

### Acceptance
- [ ] one text draft retained
"#;

    struct Fx {
        root: tempfile::TempDir,
        stop: Arc<AtomicBool>,
        threads: Vec<std::thread::JoinHandle<()>>,
        port: u16,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.stop.store(true, SeqCst);
            for thread in self.threads.drain(..).rev() {
                let result = thread.join();
                if !std::thread::panicking() {
                    result.unwrap();
                }
            }
        }
    }

    impl Fx {
        fn start() -> Self {
            let mut fx = Self {
                root: tempfile::Builder::new()
                    .prefix("c1739a-")
                    .tempdir()
                    .unwrap(),
                stop: Arc::new(AtomicBool::new(false)),
                threads: vec![],
                port: 0,
            };
            issue::Pm::init(&fx.pm()).unwrap();
            let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
            env.set("CADENCE_PM_DIR", fx.pm().to_str().unwrap());
            let opts = daemon::ServeOptions {
                provider_env: env,
                stop: Some(fx.stop.clone()),
                test_seam: true,
                slots: Some(Default::default()),
                lease: Some(Default::default()),
                auto_stop: Some(daemon::AutoStopSetting::off()),
                agent_gc: Some(Default::default()),
                report_router: Some(0),
                checkup: Some(0),
                ..Default::default()
            };
            let state = fx.state();
            fx.threads.push(std::thread::spawn(move || {
                daemon::serve_with(&state, opts).unwrap();
            }));
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2))
                    .is_ok()
                    && Seam::token_at(&fx.state()).is_some()
                {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "scratch daemon never became healthy"
                );
                // Retry the actual readiness probe, not a sleep estimating startup.
                std::thread::yield_now();
            }
            for port in 3110..3200 {
                let (tx, rx) = std::sync::mpsc::channel();
                let opts = cadence_agent::ui::ServeOpts {
                    host: "127.0.0.1".into(),
                    port,
                    stop: Some(fx.stop.clone()),
                    startup: Some(tx),
                    test_seam: true,
                    ..Default::default()
                };
                let (state, pm) = (fx.state(), fx.pm());
                let thread = std::thread::spawn(move || {
                    let _ = cadence_agent::ui::serve(&state, &pm, &opts);
                });
                match rx.recv_timeout(Duration::from_secs(20)).unwrap() {
                    Ok(()) => {
                        fx.port = port;
                        fx.threads.push(thread);
                        break;
                    }
                    Err(_) => thread.join().unwrap(),
                }
            }
            assert_ne!(fx.port, 0, "no available board port in 3110-3199");
            fx.seed_team();
            fx
        }

        fn state(&self) -> PathBuf {
            self.root.path().join("s")
        }
        fn pm(&self) -> PathBuf {
            self.root.path().join("pm")
        }
        fn db(&self) -> PathBuf {
            self.state().join("cadence.sqlite3")
        }
        fn seed_team(&self) {
            // Construction only: the same synthetic endpoint setup used by
            // cad1119; no assertion calls a Store or adapter function.
            let store = cadence_agent::store::Store::open(&self.db()).unwrap();
            for (alias, role) in [("scratch-lead", "pm"), ("scratch-writer", "worker")] {
                store
                    .register_agent(&cadence_agent::store::NewAgent {
                        alias,
                        provider: "claude",
                        endpoint_kind: "managed",
                        role,
                        cwd: self.root.path().to_str().unwrap(),
                        sandbox: "read-only",
                        instructions: None,
                        params: Some("{\"upstream\":\"scratch-lead\"}"),
                        team_role: None,
                        model_policy: None,
                    })
                    .unwrap();
                store
                    .set_identity(
                        alias,
                        &cadence_agent::adapter::Identity {
                            thread_id: "scratch-thread".into(),
                            session_id: "scratch-session".into(),
                            model: None,
                            effort: None,
                            pid: std::process::id(),
                            endpoint: None,
                            generation: Some("scratch-generation".into()),
                            attach: None,
                        },
                    )
                    .unwrap();
            }
        }
        fn bundle(&self, app: &str, version: &str) -> PathBuf {
            let source = self.root.path().join(format!("{app}-{version}"));
            std::fs::create_dir_all(source.join("workflows")).unwrap();
            std::fs::write(source.join("app.md"), format!(
                "---\napp: {app}\ntitle: Scratch app\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nSynthetic local draft fixture.\n"
            )).unwrap();
            std::fs::write(source.join("workflows/draft.md"), WORKFLOW).unwrap();
            source
        }
        fn rpc(&self, who: Asserted, method: &str, params: Value) -> Result<Value, String> {
            scoped(who, || {
                client::rpc_timeout(&self.state(), method, params, Duration::from_secs(30))
            })
            .map_err(|e| e.to_string())
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.rpc(Asserted::Operator, method, params)
                .unwrap_or_else(|e| panic!("{method}: {e}"))
        }
        fn install(&self, app: &str) -> Value {
            let source = self.bundle(app, "1.0.0");
            let checked = self.op("app_workspace_install_check", json!({"source": source}));
            let installed = self.op(
                "app_workspace_install",
                json!({
                    "source": source, "expected_digest": checked["digest"]
                }),
            );
            assert_eq!(
                installed["approved"], true,
                "fixture install consent: {installed}"
            );
            installed
        }
        fn show(&self, install: &Value) -> Value {
            self.op(
                "app_workspace_show",
                json!({"install_id": install["install_id"]}),
            )
        }
        fn proposal(&self, installed: &Value) -> Value {
            let source = self.bundle(installed["name"].as_str().unwrap(), "1.1.0");
            let check = self.op(
                "app_workspace_upgrade_check",
                json!({
                    "install_id": installed["install_id"], "source": source,
                    "expected_digest": installed["digest"],
                    "expected_generation": installed["catalog_generation"]
                }),
            );
            assert_eq!(
                check["readiness"]["ready"], true,
                "initial proposal: {check}"
            );
            json!({"install_id": installed["install_id"], "source": source,
                "expected_digest": installed["digest"],
                "expected_generation": installed["catalog_generation"],
                "expected_new_digest": check["digest"], "request_id": "scratch-upgrade"})
        }
        fn seed_derived_grant(&self, installed: &Value) {
            // Scratch historical app-derived authority; no credential, provider
            // or external effect is enrolled. Its consumer surface is the real
            // platform_grants RPC and the real upgrade-readiness guard.
            let db = rusqlite::Connection::open(self.db()).unwrap();
            db.execute("INSERT INTO app_grants(app,agent,platform,account,scopes,granted_at,by,install_id) VALUES(?,?,?,?,?,1,'scratch-fixture',?)",
                rusqlite::params!["scratch/project-app", "scratch-writer", "scratch-platform", "scratch-account", "[\"scratch:read\"]", installed["install_id"].as_str().unwrap()]).unwrap();
            db.execute("INSERT INTO platform_grants(agent,platform,account,scopes,granted_at,by) VALUES(?,?,?,?,1,'scratch-fixture')",
                rusqlite::params!["scratch-writer", "scratch-platform", "scratch-account", "[\"scratch:read\"]"]).unwrap();
        }
        fn grants(&self) -> Value {
            self.op("platform_grants", json!({"agent": "scratch-writer"}))
        }
        fn session(&self) -> (String, String) {
            cadence_agent::operator_auth::ensure_secret(&self.state()).unwrap();
            let secret = cadence_agent::operator_auth::read_secret(&self.state()).unwrap();
            let nonce = self.op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )["nonce"]
                .clone();
            let (status, text, cookie) = self.http(
                "operator",
                "/api/session",
                json!({"nonce": nonce}),
                None,
                None,
            );
            assert_eq!(status, 200, "scratch session exchange: {text}");
            let body: Value = serde_json::from_str(&text).unwrap();
            (
                cookie.unwrap(),
                body["session_key"].as_str().unwrap().into(),
            )
        }
        fn http(
            &self,
            who: &str,
            route: &str,
            body: Value,
            operation: Option<&str>,
            session: Option<&(String, String)>,
        ) -> (u16, String, Option<String>) {
            let host = format!("cadence-{}.localhost:{}", self.port, self.port);
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(35)))
                .build()
                .into();
            let mut req = agent
                .post(format!("http://127.0.0.1:{}{route}", self.port))
                .header("Host", &host)
                .header("Origin", format!("http://{host}"))
                .header("X-Cadence-Board", "1")
                .header("Content-Type", "application/json")
                .header(AS_HEADER, who)
                .header(TOKEN_HEADER, Seam::token_at(&self.state()).unwrap());
            if let Some(id) = operation {
                req = req.header("X-Cadence-Hosted-Operation-Id", id);
            }
            if let Some((cookie, key)) = session {
                req = req
                    .header("Cookie", cookie)
                    .header("X-Cadence-Session", key);
            }
            let mut response = req.send(body.to_string()).unwrap();
            let cookie = response
                .headers()
                .get("set-cookie")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap().to_owned());
            (
                response.status().as_u16(),
                response.body_mut().read_to_string().unwrap(),
                cookie,
            )
        }
        fn mutation(
            &self,
            transport: &str,
            method: &str,
            mut params: Value,
            operation: Option<&str>,
        ) -> Result<Value, String> {
            if transport == "rpc" {
                if let Some(id) = operation {
                    params["hosted_operation_id"] = json!(id);
                }
                return self.rpc(Asserted::Operator, method, params);
            }
            let id = params
                .as_object_mut()
                .unwrap()
                .remove("install_id")
                .unwrap();
            let verb = match method {
                "app_local_install_revoke" => "revoke",
                "app_workspace_upgrade" => "upgrade",
                _ => panic!("unsupported fixture mutation: {method}"),
            };
            let session = self.session();
            let route = format!("/api/app-installations/{}/{verb}", id.as_str().unwrap());
            let (status, text, _) =
                self.http("operator", &route, params, operation, Some(&session));
            if status == 200 {
                Ok(serde_json::from_str(&text)
                    .unwrap_or_else(|e| panic!("bad HTTP JSON: {e}: {text}")))
            } else {
                Err(format!("HTTP {status}: {text}"))
            }
        }
    }

    fn refusal(reply: Result<Value, String>, operation_id: &str, operation: &str) {
        let actual = reply.unwrap_or_else(|e| {
            panic!("expected correlated native refusal, not transport/error inference: {e}")
        });
        assert_eq!(
            actual,
            json!({
                "version": "hosted-app-mutation.v1", "operation_id": operation_id,
                "operation": operation, "outcome": "refused", "mutation": "not_started",
                "code": "native_precondition_refused"
            }),
            "the exact native evidence must identify this operation, not a generic error"
        );
    }

    fn unchanged_authority(fx: &Fx, installed: &Value, before: &Value, grants: &Value) {
        let after = fx.show(installed);
        assert_eq!(
            after["digest"], before["digest"],
            "refusal changed installed bytes: {after}"
        );
        assert_eq!(after["approved"], true, "refusal withdrew consent: {after}");
        assert_eq!(
            after["approval"], before["approval"],
            "refusal changed the approval epoch: {after}"
        );
        assert_eq!(
            fx.grants(),
            *grants,
            "refusal withdrew the consumer's derived grant"
        );
    }

    fn stale_generation_refuses_at_native_writer(transport: &str) {
        let fx = Fx::start();
        let installed = fx.install("scratch-stale");
        let update = fx.proposal(&installed);
        fx.seed_derived_grant(&installed);
        let grants = fx.grants();
        assert!(
            grants["grants"].as_array().unwrap().iter().any(|grant| {
                grant["platform"] == "scratch-platform"
                    && grant["scopes"] == json!(["scratch:read"])
            }),
            "scratch derived scope must actually be visible to its consumer: {grants}"
        );
        // Obtain a genuinely stale, once-valid generation by another public
        // catalog write, keeping this installation's exact bytes unchanged.
        fx.install("scratch-other");
        let before = fx.show(&installed);
        assert_ne!(
            before["catalog_generation"],
            installed["catalog_generation"]
        );
        let revoke = json!({"install_id": installed["install_id"], "digest": installed["digest"],
            "expected_generation": installed["catalog_generation"]});
        refusal(
            fx.mutation(
                transport,
                "app_local_install_revoke",
                revoke.clone(),
                Some("scratch-stale-revoke"),
            ),
            "scratch-stale-revoke",
            "revoke",
        );
        unchanged_authority(&fx, &installed, &before, &grants);
        // Direct RPC is included, not only an HTTP optimistic precheck: the
        // stale pin has to survive all the way to the PM-locked decision.
        let legacy = fx
            .mutation(transport, "app_local_install_revoke", revoke, None)
            .expect_err("uncorrelated stale revoke must remain a native error");
        assert!(
            legacy.contains("generation") && legacy.contains("stale"),
            "{legacy}"
        );
        refusal(
            fx.mutation(
                transport,
                "app_workspace_upgrade",
                update,
                Some("scratch-stale-update"),
            ),
            "scratch-stale-update",
            "update",
        );
        unchanged_authority(&fx, &installed, &before, &grants);
        // Standalone revoke intentionally has no hosted generation requirement.
        let revoked = fx
            .mutation(
                transport,
                "app_local_install_revoke",
                json!({
                    "install_id": installed["install_id"], "digest": installed["digest"]
                }),
                None,
            )
            .unwrap();
        assert_eq!(
            revoked["approved"], false,
            "standalone revoke changed: {revoked}"
        );
        assert_ne!(revoked["version"], "hosted-app-mutation.v1");
        assert_ne!(fx.show(&installed)["approved"], true);
    }

    fn active_work_is_rechecked_before_apply(transport: &str) {
        let fx = Fx::start();
        let installed = fx.install("scratch-active");
        let update = fx.proposal(&installed);
        // The proposal was ready. A real native run is admitted AFTER check;
        // apply must re-read authoritative work before it stages a journal.
        let run = fx.op(
            "app_run_create",
            json!({"install_id": installed["install_id"],
            "workflow": "draft", "inputs": {"topic": "Scratch", "writer": "scratch-writer"},
            "owner_pm": "scratch-lead", "request_id": "scratch-active-run"}),
        );
        assert_eq!(run["state"], "awaiting_approval", "real active run: {run}");
        let before = fx.show(&installed);
        refusal(
            fx.mutation(
                transport,
                "app_workspace_upgrade",
                update.clone(),
                Some("scratch-busy-update"),
            ),
            "scratch-busy-update",
            "update",
        );
        assert_eq!(fx.show(&installed)["digest"], installed["digest"]);
        assert_eq!(fx.show(&installed)["approval"], before["approval"]);
        let shown = fx.op("app_run_show", json!({"run_id": run["id"]}));
        assert_eq!(
            shown["state"], "awaiting_approval",
            "refusal must not cancel work: {shown}"
        );
        let legacy = fx
            .mutation(transport, "app_workspace_upgrade", update.clone(), None)
            .expect_err("uncorrelated busy apply must remain a native error");
        assert!(
            legacy.contains("nonterminal run"),
            "wrong native refusal: {legacy}"
        );
        fx.op("app_run_cancel", json!({"run_id": run["id"]}));
        // A refused apply left no started request/journal: the same request
        // succeeds once the operator explicitly cancels the blocking run.
        let upgraded = fx
            .mutation(transport, "app_workspace_upgrade", update.clone(), None)
            .unwrap();
        assert_eq!(
            upgraded["committed"], true,
            "standalone upgrade: {upgraded}"
        );
        assert_eq!(upgraded["digest"], update["expected_new_digest"]);
        assert_eq!(upgraded["approved"], true);
        assert_ne!(upgraded["version"], "hosted-app-mutation.v1");
    }

    fn started_mutations_never_claim_not_started(transport: &str) {
        let fx = Fx::start();
        let installed = fx.install("scratch-started");
        let update = fx.proposal(&installed);
        // Fault construction only. The old bundle and preflight remain valid;
        // this collision is first encountered when delivery creates the new
        // revision, AFTER staging the actual journal and pending marker.
        let obstruction = fx
            .pm()
            .join(".apps/installations")
            .join(installed["install_id"].as_str().unwrap())
            .join("revisions");
        std::fs::write(&obstruction, "scratch delivery obstruction\n").unwrap();
        let failure = fx
            .mutation(
                transport,
                "app_workspace_upgrade",
                update.clone(),
                Some("scratch-started-update"),
            )
            .expect_err("a journaled delivery failure is NOT pre-mutation refusal evidence");
        assert!(
            failure.contains("delivery incomplete") && failure.contains("upgrade-recover"),
            "{failure}"
        );
        assert!(
            !failure.contains("not_started"),
            "false no-mutation assurance: {failure}"
        );
        std::fs::remove_file(obstruction).unwrap();
        // Public recovery proves a genuine retained native journal exists;
        // no assertion inspects journal files or native forwarding calls.
        let recovered = fx.op(
            "app_workspace_upgrade_recover",
            json!({
                "install_id": installed["install_id"], "request_id": update["request_id"]
            }),
        );
        assert_eq!(
            recovered["committed"], true,
            "retained operation must recover: {recovered}"
        );
        assert_eq!(recovered["digest"], update["expected_new_digest"]);
        assert_eq!(fx.show(&installed)["digest"], update["expected_new_digest"]);

        // A native store decision can also fail after entering the writer.
        // Inject a scratch SQL refusal, not a replacement native response.
        let current = fx.show(&installed);
        let db = rusqlite::Connection::open(fx.db()).unwrap();
        db.execute_batch("CREATE TRIGGER scratch_refuse_revoke BEFORE UPDATE ON app_install_capabilities BEGIN SELECT RAISE(ABORT, 'scratch consent writer failure'); END;").unwrap();
        let failure = fx
            .mutation(
                transport,
                "app_local_install_revoke",
                json!({
                    "install_id": current["install_id"], "digest": current["digest"],
                    "expected_generation": current["catalog_generation"]
                }),
                Some("scratch-started-revoke"),
            )
            .expect_err("a failed native consent transaction must not claim not_started");
        assert!(
            !failure.contains("not_started"),
            "false no-mutation assurance: {failure}"
        );
        assert!(
            failure.contains("scratch consent writer failure"),
            "wrong writer failure: {failure}"
        );
        db.execute_batch("DROP TRIGGER scratch_refuse_revoke;")
            .unwrap();
        let revoked = fx
            .mutation(
                transport,
                "app_local_install_revoke",
                json!({
                    "install_id": current["install_id"], "digest": current["digest"]
                }),
                None,
            )
            .unwrap();
        assert_eq!(
            revoked["approved"], false,
            "standalone writer after fault removal: {revoked}"
        );
    }

    fn correlation_is_not_operator_authority() {
        let fx = Fx::start();
        let installed = fx.install("scratch-auth");
        let update = fx.proposal(&installed);
        let before = fx.show(&installed);
        let session = fx.session();
        for (method, mut params, verb) in [
            ("app_workspace_upgrade", update, "upgrade"),
            (
                "app_local_install_revoke",
                json!({"install_id": installed["install_id"],
                "digest": installed["digest"], "expected_generation": installed["catalog_generation"]}),
                "revoke",
            ),
        ] {
            params["hosted_operation_id"] = json!("scratch-forged-authority");
            for who in [Asserted::Unproven, Asserted::Agent("scratch-writer".into())] {
                let error = fx
                    .rpc(who, method, params.clone())
                    .expect_err("correlation cannot confer operator authority");
                assert!(
                    error.contains("operator"),
                    "wrong native caller refusal: {error}"
                );
                assert!(!error.contains("not_started"));
            }
            let object = params.as_object_mut().unwrap();
            object.remove("hosted_operation_id");
            object.remove("install_id");
            let route = format!(
                "/api/app-installations/{}/{verb}",
                installed["install_id"].as_str().unwrap()
            );
            // Header alone, plus an agent attempting to borrow a real scratch
            // operator cookie: neither crosses the board or daemon guard.
            for (who, credential) in [("unproven", None), ("agent:scratch-writer", Some(&session))]
            {
                let (status, text, _) = fx.http(
                    who,
                    &route,
                    params.clone(),
                    Some("scratch-forged-authority"),
                    credential,
                );
                assert_eq!(
                    status, 403,
                    "operation header bypassed HTTP caller guard: {text}"
                );
                assert!(
                    !text.contains("not_started"),
                    "auth failure is not native mutation evidence: {text}"
                );
            }
        }
        let after = fx.show(&installed);
        assert_eq!(after["digest"], before["digest"]);
        assert_eq!(after["approval"], before["approval"]);
        assert_eq!(after["approved"], true);
    }

    #[test]
    fn hosted_app_mutations_preserve_native_guards() {
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::Builder::new()
                .prefix("c1739home-")
                .tempdir_in("/tmp")
                .unwrap();
            for dir in ["home", "tmp", "state", "config", "data", "cache", "locks"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
                .env(CHILD, "1")
                .env("HOME", root.path().join("home"))
                .env("XDG_STATE_HOME", root.path().join("state"))
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("XDG_CACHE_HOME", root.path().join("cache"))
                .env("TMPDIR", root.path().join("tmp"))
                .env("CADENCE_PM_DIR", root.path().join("pm"))
                .env("CADENCE_STATE_DIR", root.path().join("s"))
                .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite.lock"))
                .env_remove("CADENCE_ALIAS")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", root.path().join("home/gitconfig"));
            let home = std::env::var_os("HOME").unwrap();
            for (key, fallback) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
                command.env(
                    key,
                    std::env::var_os(key)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| Path::new(&home).join(fallback)),
                );
            }
            let output = cadence_agent::reaper::output(&mut command).unwrap();
            assert!(
                output.status.success(),
                "isolated CAD-1739 app acceptance failed ({:?}):\n{}\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            println!("{}", String::from_utf8_lossy(&output.stdout));
            return;
        }
        correlation_is_not_operator_authority();
        println!("PASS correlation never grants RPC/HTTP operator authority");
        for transport in ["rpc", "http"] {
            stale_generation_refuses_at_native_writer(transport);
            println!("PASS {transport}: stale revoke/update refuse exactly; approval and derived scopes survive; standalone revoke unchanged");
            active_work_is_rechecked_before_apply(transport);
            println!("PASS {transport}: newly active run refuses apply exactly; standalone busy error and later update unchanged");
            started_mutations_never_claim_not_started(transport);
            println!("PASS {transport}: journaled update recovers; entered consent writer failure never claims not_started");
        }
    }
}
