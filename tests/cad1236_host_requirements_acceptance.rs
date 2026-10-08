//! CAD-1236 reviewer-owned acceptance, derived from the ticket, not the
//! implementation. Real daemon + real HTTP peer, explicit operator proof.
//! Refusals must name the requirement and preserve the whole tracker,
//! including catalog generation, bundle bytes, records and journal directories.
//! Run with `--features test-seam`; without it the test fails rather than
//! silently reporting zero acceptance checks.

#[cfg(not(feature = "test-seam"))]
#[test]
fn host_requirements_refuse_incompatible_packages_without_writes() {
    panic!("CAD-1236 acceptance requires --features test-seam (real daemon/operator fixture)");
}

#[cfg(all(feature = "test-seam", target_os = "linux"))]
mod acceptance {
    use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
    use cadence_agent::{client, daemon, issue};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const TEST: &str = "acceptance::host_requirements_refuse_incompatible_packages_without_writes";
    const CHILD: &str = "CADENCE_CAD1236_ACCEPT_CHILD";
    const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}}\"\ninputs:\n  topic: { ask: \"About what?\" }\n---\n\nWhy.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n";

    fn manifest(app: &str, version: &str, requires: &str) -> String {
        format!("---\napp: {app}\ntitle: Host Requirements\nversion: '{version}'\nneeds:\n  connections: []\n{requires}---\n\nGuide.\n")
    }

    /// Exact bundle-byte pin, independent of manifest admission. A rejected
    /// check cannot issue a receipt, so use its digest when it does answer
    /// (pre-gate baseline), otherwise use this byte pin. Positive controls
    /// verify the pin against a real successful install-check receipt.
    fn byte_digest(src: &Path) -> String {
        let mut hash = Sha256::new();
        for name in ["app.md", "workflows/do.md"] {
            let body = std::fs::read(src.join(name)).unwrap();
            hash.update((name.len() as u64).to_be_bytes());
            hash.update(name.as_bytes());
            hash.update((body.len() as u64).to_be_bytes());
            hash.update(body);
        }
        format!("sha256:{:x}", hash.finalize())
    }

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
                thread.join().unwrap();
            }
        }
    }

    impl Fx {
        fn start() -> Self {
            let root = tempfile::Builder::new().prefix("c1236-").tempdir().unwrap();
            let mut fx = Self {
                root,
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
                daemon::serve_with(&state, opts).unwrap()
            }));
            let deadline = Instant::now() + Duration::from_secs(30);
            while client::rpc_timeout(&fx.state(), "health", json!({}), Duration::from_secs(2))
                .is_err()
                || Seam::token_at(&fx.state()).is_none()
            {
                assert!(Instant::now() < deadline, "daemon never started");
                std::thread::sleep(Duration::from_millis(50));
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
            assert_ne!(fx.port, 0, "no board port in 3110-3199");
            fx
        }
        fn state(&self) -> PathBuf {
            self.root.path().join("s")
        }
        fn pm(&self) -> PathBuf {
            self.root.path().join("pm")
        }
        fn bundle(&self, app: &str, version: &str, requires: &str) -> PathBuf {
            let dir = self.root.path().join("src").join(app);
            std::fs::create_dir_all(dir.join("workflows")).unwrap();
            std::fs::write(dir.join("app.md"), manifest(app, version, requires)).unwrap();
            std::fs::write(dir.join("workflows/do.md"), WORKFLOW).unwrap();
            dir
        }
        fn rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
            scoped(Asserted::Operator, || {
                client::rpc(&self.state(), method, params)
            })
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.rpc(method, params)
                .unwrap_or_else(|e| panic!("{method}: {e}"))
        }
        fn tree(&self) -> BTreeMap<String, Vec<u8>> {
            fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
                for entry in std::fs::read_dir(dir).unwrap() {
                    let p = entry.unwrap().path();
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                    if rel == ".git" {
                        continue;
                    }
                    if p.is_dir() {
                        out.insert(format!("{rel}/"), vec![]);
                        walk(base, &p, out);
                    } else {
                        out.insert(rel, std::fs::read(p).unwrap());
                    }
                }
            }
            let mut out = BTreeMap::new();
            walk(&self.pm(), &self.pm(), &mut out);
            let head = cadence_agent::reaper::output(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(self.pm())
                    .args(["rev-parse", "HEAD"]),
            )
            .unwrap();
            assert!(head.status.success());
            out.insert("<git HEAD>".into(), head.stdout);
            out
        }
        /// Read consent rows and approval audit evidence without opening a
        /// second writer or running store migration/recovery against the daemon.
        fn consent_snapshot(&self, id: &str) -> Value {
            let conn = rusqlite::Connection::open_with_flags(
                self.state().join("cadence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let rows = conn
                .prepare(
                    "SELECT 'current',epoch,digest,state,created FROM app_install_capabilities WHERE install_id=?
                     UNION ALL
                     SELECT 'history',epoch,digest,state,created FROM app_capability_epochs WHERE install_id=?
                     ORDER BY 1,2",
                )
                .unwrap()
                .query_map([id, id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            let approvals = conn
                .prepare(
                    "SELECT seq,payload,at FROM events
                     WHERE kind='app_install_capability_approved'
                     AND json_extract(payload,'$.install_id')=? ORDER BY seq",
                )
                .unwrap()
                .query_map([id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            json!({"rows": rows, "approvals": approvals})
        }
        fn session(&self) -> (String, String) {
            cadence_agent::operator_auth::ensure_secret(&self.state()).unwrap();
            let secret = cadence_agent::operator_auth::read_secret(&self.state()).unwrap();
            let nonce = self.op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )["nonce"]
                .clone();
            let (status, text, cookie) =
                self.http_with("/api/session", &json!({"nonce": nonce}).to_string(), None);
            assert_eq!(status, 200, "operator session: {text}");
            let body: Value = serde_json::from_str(&text).unwrap();
            (
                cookie.unwrap(),
                body["session_key"].as_str().unwrap().into(),
            )
        }
        fn http(&self, route: &str, body: Value) -> (u16, String) {
            let session = self.session();
            let (status, text, _) = self.http_with(route, &body.to_string(), Some(&session));
            (status, text)
        }
        fn http_with(
            &self,
            route: &str,
            body: &str,
            session: Option<&(String, String)>,
        ) -> (u16, String, Option<String>) {
            let host = format!("cadence-{}.localhost:{}", self.port, self.port);
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let token = Seam::token_at(&self.state()).unwrap();
            let mut req = agent
                .post(format!("http://127.0.0.1:{}{route}", self.port))
                .header("Host", &host)
                .header("Origin", format!("http://{host}"))
                .header("X-Cadence-Board", "1")
                .header("Content-Type", "application/json")
                .header(AS_HEADER, "operator")
                .header(TOKEN_HEADER, token);
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
                .map(|v| v.split(';').next().unwrap().to_string());
            (
                response.status().as_u16(),
                response.body_mut().read_to_string().unwrap(),
                cookie,
            )
        }
    }

    fn record(errors: &mut Vec<String>, label: &str, ok: bool, detail: impl std::fmt::Display) {
        if ok {
            println!("PASS {label}");
        } else {
            errors.push(format!("FAIL {label}: {detail}"));
        }
    }

    fn refusal(
        errors: &mut Vec<String>,
        label: &str,
        reply: Result<Value, String>,
        requirement: &str,
    ) {
        let ok = match &reply {
            Err(text) => text.to_lowercase().contains("requires") && text.contains(requirement),
            Ok(_) => false,
        };
        record(
            errors,
            label,
            ok,
            format!("expected refusal naming requires/{requirement}, got {reply:?}"),
        );
    }

    fn unchanged(
        fx: &Fx,
        errors: &mut Vec<String>,
        label: &str,
        tree: &BTreeMap<String, Vec<u8>>,
        catalog: &Value,
    ) {
        record(
            errors,
            &format!("{label} tracker unchanged"),
            &fx.tree() == tree,
            "catalog/bundle/record/journal directories or tracker HEAD changed",
        );
        let after = fx.op("app_workspace_list", json!({}));
        record(
            errors,
            &format!("{label} catalog unchanged"),
            &after == catalog,
            format!("new installation or catalog generation moved: {after}"),
        );
    }

    fn http_reply(status: u16, text: String) -> Result<Value, String> {
        if (400..500).contains(&status) {
            Err(text)
        } else {
            Ok(json!({"status": status, "text": text}))
        }
    }

    fn positive(fx: &Fx, app: &str, requires: &str) -> Value {
        let src = fx.bundle(app, "1.0.0", requires);
        let before = fx.tree();
        let check = fx.op("app_workspace_install_check", json!({"source": src}));
        assert_eq!(
            fx.tree(),
            before,
            "positive install-check must be read-only"
        );
        assert_eq!(
            check["digest"],
            byte_digest(&src),
            "pin calculation must match the real check"
        );
        let (status, text) = fx.http(
            "/api/app-installations",
            json!({"source": src, "expected_digest": check["digest"]}),
        );
        assert_eq!(status, 200, "positive HTTP install {app}: {text}");
        let installed: Value = serde_json::from_str(&text).unwrap();
        let shown = fx.op(
            "app_workspace_show",
            json!({"install_id": installed["install_id"]}),
        );
        assert_eq!(shown["digest"], check["digest"]);
        assert_eq!(shown["version"], "1.0.0");
        assert!(!installed["catalog_generation"].is_null(), "{installed}");
        println!("PASS positive {app}: check and HTTP install, digest recorded");
        installed
    }

    #[test]
    fn host_requirements_refuse_incompatible_packages_without_writes() {
        // Re-exec the one acceptance check with a short isolated HOME/XDG/
        // TMPDIR before starting any fixture; no global environment edits
        // while daemon threads run and no dependency on an agent's ancestry.
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::Builder::new()
                .prefix("c1236home-")
                .tempdir_in("/tmp")
                .unwrap();
            for dir in ["home", "tmp", "xdg", "config", "data", "locks"] {
                std::fs::create_dir_all(root.path().join(dir)).unwrap();
            }
            let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
            cmd.args(["--exact", TEST, "--test-threads", "1", "--nocapture"])
                .env(CHILD, "1")
                .env("HOME", root.path().join("home"))
                .env("XDG_STATE_HOME", root.path().join("xdg"))
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("TMPDIR", root.path().join("tmp"))
                .env("CADENCE_PM_DIR", root.path().join("pm"))
                .env("CADENCE_STATE_DIR", root.path().join("s"))
                .env("CADENCE_SUITE_LOCK", root.path().join("locks/suite.lock"));
            let home = std::env::var_os("HOME").unwrap();
            for (key, fallback) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
                cmd.env(
                    key,
                    std::env::var_os(key)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| Path::new(&home).join(fallback)),
                );
            }
            let output = cadence_agent::reaper::output(&mut cmd).unwrap();
            assert!(
                output.status.success(),
                "isolated acceptance failed ({:?}):\n{}\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            println!("{}", String::from_utf8_lossy(&output.stdout));
            return;
        }

        let mut errors = vec![];
        let cases = [
            ("core", "  requires:\n    core: '>=99.0.0'\n", "core"),
            (
                "contract-major",
                "  requires:\n    contracts: {app-views: [99]}\n",
                "app-views",
            ),
            ("bad-range", "  requires:\n    core: 'banana'\n", "core"),
            (
                "unknown-key",
                "  requires:\n    unknown-host-key: true\n",
                "unknown-host-key",
            ),
            (
                "unknown-contract",
                "  requires:\n    contracts: {unknown-host-contract: [1]}\n",
                "unknown-host-contract",
            ),
        ];
        for (name, requires, requirement) in cases {
            let fx = Fx::start();
            // An existing legacy installation makes generation comparisons
            // meaningful and ensures catalog reads cannot lazily create it.
            positive(&fx, "legacy", "");
            let tree = fx.tree();
            let catalog = fx.op("app_workspace_list", json!({}));
            for (transport, app) in [("rpc", "bad-rpc"), ("http", "bad-http")] {
                let src = fx.bundle(app, "2.0.0", requires);
                let label = format!("{name} {transport}");
                let check = if transport == "rpc" {
                    fx.rpc("app_workspace_install_check", json!({"source": src}))
                        .map_err(|e| e.to_string())
                } else {
                    let (status, text) =
                        fx.http("/api/app-installations/check", json!({"source": src}));
                    http_reply(status, text)
                };
                // Before implementation, install-check answers; use that
                // receipt's pin. After implementation, it must refuse, and
                // the exact-byte fallback still reaches the compatibility
                // gate instead of a missing/mismatched digest guard.
                let digest = check
                    .as_ref()
                    .ok()
                    .and_then(|v| v.get("digest"))
                    .cloned()
                    .unwrap_or_else(|| json!(byte_digest(&src)));
                refusal(
                    &mut errors,
                    &format!("{label} install-check"),
                    check,
                    requirement,
                );
                unchanged(&fx, &mut errors, &format!("{label} check"), &tree, &catalog);
                let params = json!({"source": src, "expected_digest": digest});
                let reply = if transport == "rpc" {
                    fx.rpc("app_workspace_install", params)
                        .map_err(|e| e.to_string())
                } else {
                    let (status, text) = fx.http("/api/app-installations", params);
                    http_reply(status, text)
                };
                refusal(&mut errors, &format!("{label} install"), reply, requirement);
                unchanged(
                    &fx,
                    &mut errors,
                    &format!("{label} install"),
                    &tree,
                    &catalog,
                );
            }
        }

        let fx = Fx::start();
        // Explicit prerelease comparator admits 0.1.0-beta.2; >=0.0.1
        // alone excludes prereleases under conventional semver rules.
        positive(
            &fx,
            "compatible",
            "  requires:\n    core: '>=0.1.0-beta.2 <2.0.0'\n    contracts: {app-views: [1]}\n",
        );
        let installed = positive(&fx, "upgrade-probe", "");
        let id = installed["install_id"].as_str().unwrap();
        let shown = fx.op("app_workspace_show", json!({"install_id": id}));
        let bindings = fx.op("app_binding_list", json!({"install_id": id}));
        let src = fx.bundle(
            "upgrade-probe",
            "2.0.0",
            "  requires:\n    core: '>=99.0.0'\n",
        );
        let tree = fx.tree();
        let catalog = fx.op("app_workspace_list", json!({}));
        let params = json!({"install_id": id, "source": src,
            "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"]});
        for transport in ["rpc", "http"] {
            let label = format!("upgrade core {transport}");
            let check = if transport == "rpc" {
                fx.rpc("app_workspace_upgrade_check", params.clone())
                    .map_err(|e| e.to_string())
            } else {
                let mut body = params.clone();
                body.as_object_mut().unwrap().remove("install_id");
                let (status, text) =
                    fx.http(&format!("/api/app-installations/{id}/upgrade/check"), body);
                http_reply(status, text)
            };
            refusal(
                &mut errors,
                &format!("{label} upgrade-check"),
                check,
                "core",
            );
            unchanged(&fx, &mut errors, &format!("{label} check"), &tree, &catalog);
            let mut upgrade = params.clone();
            upgrade["expected_new_digest"] = json!(byte_digest(&src));
            upgrade["request_id"] = json!(format!("up-c1236-{transport}"));
            let reply = if transport == "rpc" {
                fx.rpc("app_workspace_upgrade", upgrade)
                    .map_err(|e| e.to_string())
            } else {
                upgrade.as_object_mut().unwrap().remove("install_id");
                let (status, text) =
                    fx.http(&format!("/api/app-installations/{id}/upgrade"), upgrade);
                http_reply(status, text)
            };
            refusal(&mut errors, &format!("{label} upgrade"), reply, "core");
            unchanged(&fx, &mut errors, &label, &tree, &catalog);
            record(
                &mut errors,
                &format!("{label} installed digest/version/context unchanged"),
                fx.op("app_workspace_show", json!({"install_id": id})) == shown,
                "installed record changed",
            );
            record(
                &mut errors,
                &format!("{label} bindings unchanged"),
                fx.op("app_binding_list", json!({"install_id": id})) == bindings,
                "bindings changed",
            );
        }
        drop(fx);

        // Each transport gets its own fixture: a broken recovery must not
        // leave a pending marker that masks the next transport's real guard.
        for transport in ["rpc", "http"] {
            let fx = Fx::start();
            let installed = positive(&fx, "recover-probe", "");
            let id = installed["install_id"].as_str().unwrap();
            let src = fx.bundle("recover-probe", "2.0.0", "");
            let request = "recovery-probe";
            let mut params = json!({"install_id": id, "source": src,
                "expected_digest": installed["digest"],
                "expected_generation": installed["catalog_generation"]});
            let check = fx.op("app_workspace_upgrade_check", params.clone());
            params["expected_new_digest"] = check["digest"].clone();
            params["request_id"] = json!(request);
            fx.op("app_workspace_upgrade", params);

            // Model an internally consistent retained journal produced by a
            // newer host, now resumed on this host. Re-pin its exact bytes and
            // destination revision; otherwise a digest error could mask the
            // missing host requirement guard. Put publication back at "before"
            // with no pending marker, the retained-journal recovery case.
            let journal_path = fx
                .pm()
                .join(".apps/upgrade-journals")
                .join(id)
                .join(format!("{request}.yaml"));
            let mut journal: Value =
                serde_yaml::from_str(&std::fs::read_to_string(&journal_path).unwrap()).unwrap();
            journal["files"]["app.md"] = json!(manifest(
                "recover-probe",
                "2.0.0",
                "  requires:\n    core: '>=99.0.0'\n",
            ));
            let files: BTreeMap<String, String> =
                serde_json::from_value(journal["files"].clone()).unwrap();
            let mut hash = Sha256::new();
            for (name, body) in files {
                hash.update((name.len() as u64).to_be_bytes());
                hash.update(name.as_bytes());
                hash.update((body.len() as u64).to_be_bytes());
                hash.update(body.as_bytes());
            }
            let digest = format!("sha256:{:x}", hash.finalize());
            journal["expected_new_digest"] = json!(digest);
            let mut after: Value =
                serde_yaml::from_str(journal["after_catalog"].as_str().unwrap()).unwrap();
            after["installations"][id]["bundle_revision"] =
                json!(digest.trim_start_matches("sha256:"));
            journal["after_catalog"] = json!(serde_yaml::to_string(&after).unwrap());
            std::fs::write(&journal_path, serde_yaml::to_string(&journal).unwrap()).unwrap();
            std::fs::write(
                fx.pm().join(".apps/catalog.yaml"),
                journal["before_catalog"].as_str().unwrap(),
            )
            .unwrap();
            std::fs::write(
                fx.pm()
                    .join(".apps/installations")
                    .join(id)
                    .join("record.yaml"),
                journal["before_record"].as_str().unwrap(),
            )
            .unwrap();
            let pending = fx.pm().join(".apps/upgrade-pending.yaml");
            assert!(
                !pending.exists(),
                "recovery fixture starts without a marker"
            );
            let tree = fx.tree();
            let catalog = fx.op("app_workspace_list", json!({}));
            let label = format!("recover core {transport}");
            let reply = if transport == "rpc" {
                fx.rpc(
                    "app_workspace_upgrade_recover",
                    json!({"install_id": id, "request_id": request}),
                )
                .map_err(|e| e.to_string())
            } else {
                let (status, text) = fx.http(
                    &format!("/api/app-installations/{id}/upgrade/recover"),
                    json!({"request_id": request}),
                );
                http_reply(status, text)
            };
            refusal(&mut errors, &label, reply, "core");
            record(
                &mut errors,
                &format!("{label} tracker unchanged"),
                fx.tree() == tree,
                "recovery changed tracker bytes/directories or Git HEAD",
            );
            record(
                &mut errors,
                &format!("{label} no pending marker"),
                !pending.exists(),
                "recovery wrote .apps/upgrade-pending.yaml before refusing",
            );
            // Do not panic if the broken path leaves catalog reads blocked:
            // accumulate that failure so restore and both transports still run.
            let after = fx.rpc("app_workspace_list", json!({}));
            record(
                &mut errors,
                &format!("{label} catalog remains readable and unchanged"),
                after.as_ref().is_ok_and(|value| value == &catalog),
                format!("catalog changed or became unreadable: {after:?}"),
            );
        }

        for transport in ["rpc", "http"] {
            let fx = Fx::start();
            let installed = positive(&fx, "restore-probe", "");
            let id = installed["install_id"].as_str().unwrap();
            let removed = fx.op(
                "app_workspace_remove",
                json!({"install_id": id, "expected_digest": installed["digest"],
                    "expected_generation": installed["catalog_generation"],
                    "request_id": "remove-restore-probe"}),
            );
            assert_eq!(removed["state"], "removed");
            // Model a removed package persisted by a newer host. Reads must
            // report incompatibility, while restore must refuse re-admission.
            std::fs::write(
                fx.pm()
                    .join(".apps/installations")
                    .join(id)
                    .join("bundle/app.md"),
                manifest(
                    "restore-probe",
                    "1.0.0",
                    "  requires:\n    core: '>=99.0.0'\n",
                ),
            )
            .unwrap();
            let shown = fx.op("app_workspace_show", json!({"install_id": id}));
            assert_eq!(shown["compatibility"]["ok"], false);
            assert!(shown["removed"].as_i64().is_some());
            assert_ne!(shown["approval"]["state"], "approved");
            let tree = fx.tree();
            let catalog = fx.op("app_workspace_list", json!({}));
            let consent = fx.consent_snapshot(id);
            let label = format!("restore core {transport}");
            let reply = if transport == "rpc" {
                fx.rpc("app_workspace_restore", json!({"install_id": id}))
                    .map_err(|e| e.to_string())
            } else {
                let (status, text) =
                    fx.http(&format!("/api/app-installations/{id}/restore"), json!({}));
                http_reply(status, text)
            };
            refusal(&mut errors, &label, reply, "core");
            unchanged(&fx, &mut errors, &label, &tree, &catalog);
            record(
                &mut errors,
                &format!("{label} no consent recorded"),
                fx.consent_snapshot(id) == consent,
                "consent row, approval epoch or approval audit evidence changed",
            );
        }

        assert!(
            errors.is_empty(),
            "CAD-1236 acceptance failures:\n{}",
            errors.join("\n")
        );
    }
}
