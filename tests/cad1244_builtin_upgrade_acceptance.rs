//! CAD-1244 reviewer-owned acceptance, derived from the ticket and PM scope.
//! Builtin upgrades must work without weakening app, pin or caller guards.
//! Real daemon and board HTTP; refusals preserve tracker bytes and catalog.
//! Only this file is QA-owned; the implementer must not edit or weaken it.

#[cfg(not(all(feature = "test-seam", target_os = "linux")))]
#[test]
fn builtin_upgrade_preserves_existing_guards() {
    panic!("CAD-1244 acceptance requires Linux and --features test-seam");
}

#[cfg(all(feature = "test-seam", target_os = "linux"))]
mod acceptance {
    use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
    use cadence_agent::{client, daemon, issue};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const TEST: &str = "acceptance::builtin_upgrade_preserves_existing_guards";
    const CHILD: &str = "CADENCE_CAD1244_ACCEPT_CHILD";
    const WRONG: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

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
            let root = tempfile::Builder::new().prefix("c1244-").tempdir().unwrap();
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
        fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
            scoped(who, || client::rpc(&self.state(), method, params))
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.rpc(Asserted::Operator, method, params)
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
        fn session(&self) -> (String, String) {
            cadence_agent::operator_auth::ensure_secret(&self.state()).unwrap();
            let secret = cadence_agent::operator_auth::read_secret(&self.state()).unwrap();
            let nonce = self.op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )["nonce"]
                .clone();
            let (status, text, cookie) = self.http_with(
                "operator",
                "/api/session",
                &json!({"nonce": nonce}).to_string(),
                None,
            );
            assert_eq!(status, 200, "operator session: {text}");
            let body: Value = serde_json::from_str(&text).unwrap();
            (
                cookie.unwrap(),
                body["session_key"].as_str().unwrap().into(),
            )
        }
        fn http(&self, who: &str, route: &str, body: Value) -> (u16, String) {
            let session = (who == "operator").then(|| self.session());
            let (status, text, _) = self.http_with(who, route, &body.to_string(), session.as_ref());
            (status, text)
        }
        fn http_with(
            &self,
            who: &str,
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
                .header(AS_HEADER, who)
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
            let failure = format!("FAIL {label}: {detail}");
            println!("{failure}");
            errors.push(failure);
        }
    }

    fn call(
        fx: &Fx,
        transport: &str,
        who: Asserted,
        apply: bool,
        mut params: Value,
    ) -> Result<Value, String> {
        if transport == "rpc" {
            let method = if apply {
                "app_workspace_upgrade"
            } else {
                "app_workspace_upgrade_check"
            };
            fx.rpc(who, method, params).map_err(|e| e.to_string())
        } else {
            let id = params
                .as_object_mut()
                .unwrap()
                .remove("install_id")
                .unwrap();
            let suffix = if apply { "upgrade" } else { "upgrade/check" };
            let route = format!("/api/app-installations/{}/{suffix}", id.as_str().unwrap());
            let (status, text) = fx.http(&who.as_str(), &route, params);
            if (400..500).contains(&status) {
                Err(text)
            } else if status == 200 {
                serde_json::from_str(&text)
                    .map_err(|e| format!("invalid success JSON: {e}: {text}"))
            } else {
                panic!("unexpected HTTP status {status}: {text}");
            }
        }
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
            "tracker files/directories, catalog, bundles, records, journals or Git HEAD changed",
        );
        let after = fx.rpc(Asserted::Operator, "app_workspace_list", json!({}));
        record(
            errors,
            &format!("{label} catalog unchanged"),
            after.as_ref().is_ok_and(|value| value == catalog),
            format!("catalog changed/unreadable: {after:?}"),
        );
    }

    fn refusal(
        fx: &Fx,
        errors: &mut Vec<String>,
        transport: &str,
        who: Asserted,
        apply: bool,
        params: Value,
        case: &str,
    ) {
        let catalog = fx.op("app_workspace_list", json!({}));
        let tree = fx.tree();
        let label = format!(
            "{case} {transport} {}",
            if apply { "apply" } else { "check" }
        );
        let reply = call(fx, transport, who, apply, params);
        // A generic unsupported-source/session rejection is not evidence that
        // the intended operator guard or pin/app guard actually ran.
        let ok = reply.as_ref().err().is_some_and(|text| {
            let text = text.to_lowercase();
            match case {
                "wrong-app" => {
                    text.contains("app")
                        && (text.contains("match")
                            || text.contains("differ")
                            || text.contains("rename")
                            || (text.contains("identity") && text.contains("change")))
                }
                "stale-digest" => {
                    text.contains("digest")
                        && (text.contains("stale")
                            || text.contains("match")
                            || text.contains("changed"))
                }
                "stale-generation" => text.contains("generation") && text.contains("stale"),
                "wrong-new-digest" => {
                    text.contains("digest")
                        && (text.contains("check")
                            || text.contains("match")
                            || text.contains("changed"))
                }
                "unknown-builtin" => {
                    text.contains("unknown")
                        && (text.contains("catalog")
                            || text.contains("builtin")
                            || text.contains("built-in"))
                }
                "agent" | "unproven" => text.contains("operator"),
                _ => false,
            }
        });
        record(
            errors,
            &label,
            ok,
            format!("expected {case} refusal, got {reply:?}"),
        );
        unchanged(fx, errors, &label, &tree, &catalog);
    }

    /// Copy the actual CRM package, not a fake mirroring the implementation.
    /// Change only version and guide; install-check calibrates both byte pins.
    fn older_crm(fx: &Fx) -> PathBuf {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let dest = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy(&entry.path(), &dest);
                } else {
                    std::fs::copy(entry.path(), dest).unwrap();
                }
            }
        }
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("workspace-apps/crm");
        let dest = fx.root.path().join("older-crm");
        copy(&src, &dest);
        let manifest = std::fs::read_to_string(dest.join("app.md")).unwrap();
        let mut changed = false;
        let mut old = manifest
            .lines()
            .map(|line| {
                if line.starts_with("version:") {
                    changed = true;
                    "version: '0.0.1'"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(changed, "CRM fixture needs a version field");
        old.push_str("\n\nOlder CRM guide for CAD-1244 acceptance.\n");
        std::fs::write(dest.join("app.md"), old).unwrap();
        dest
    }

    #[test]
    fn builtin_upgrade_preserves_existing_guards() {
        // Re-exec the one acceptance check with a short isolated HOME/XDG/
        // TMPDIR before starting any fixture; no global environment edits
        // while daemon threads run and no dependency on an agent's ancestry.
        if std::env::var_os(CHILD).is_none() {
            let root = tempfile::Builder::new()
                .prefix("c1244home-")
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
        for transport in ["rpc", "http"] {
            let fx = Fx::start();
            let src = older_crm(&fx);
            let old_check = fx.op("app_workspace_install_check", json!({"source": src}));
            let builtin = fx.op(
                "app_workspace_install_check",
                json!({"source": "builtin:crm"}),
            );
            let other = fx.op(
                "app_workspace_install_check",
                json!({"source": "builtin:social-content"}),
            );
            assert_ne!(
                old_check["digest"], builtin["digest"],
                "fixture must require a real upgrade"
            );
            let installed = fx.op(
                "app_workspace_install",
                json!({"source": src, "expected_digest": old_check["digest"]}),
            );
            let id = installed["install_id"].as_str().unwrap();
            let params = json!({"install_id": id, "source": "builtin:crm",
                "expected_digest": installed["digest"], "expected_generation": installed["catalog_generation"]});
            let mut apply = params.clone();
            apply["expected_new_digest"] = builtin["digest"].clone();
            apply["request_id"] = json!(format!("cad1244-{transport}"));

            for (case, key, value) in [
                ("wrong-app", "source", json!("builtin:social-content")),
                ("stale-digest", "expected_digest", json!(WRONG)),
                ("stale-generation", "expected_generation", json!(WRONG)),
                ("unknown-builtin", "source", json!("builtin:does-not-exist")),
            ] {
                for is_apply in [false, true] {
                    let mut bad = if is_apply {
                        apply.clone()
                    } else {
                        params.clone()
                    };
                    bad[key] = value.clone();
                    if case == "wrong-app" && is_apply {
                        bad["expected_new_digest"] = other["digest"].clone();
                    }
                    refusal(
                        &fx,
                        &mut errors,
                        transport,
                        Asserted::Operator,
                        is_apply,
                        bad,
                        case,
                    );
                }
            }
            let mut bad = apply.clone();
            bad["expected_new_digest"] = json!(WRONG);
            refusal(
                &fx,
                &mut errors,
                transport,
                Asserted::Operator,
                true,
                bad,
                "wrong-new-digest",
            );
            for (case, who) in [
                ("agent", Asserted::Agent("writer".into())),
                ("unproven", Asserted::Unproven),
            ] {
                for is_apply in [false, true] {
                    refusal(
                        &fx,
                        &mut errors,
                        transport,
                        who.clone(),
                        is_apply,
                        if is_apply {
                            apply.clone()
                        } else {
                            params.clone()
                        },
                        case,
                    );
                }
            }

            let catalog = fx.op("app_workspace_list", json!({}));
            let tree = fx.tree();
            let proposal = call(&fx, transport, Asserted::Operator, false, params);
            record(
                &mut errors,
                &format!("positive {transport} proposal matches builtin install-check"),
                proposal
                    .as_ref()
                    .is_ok_and(|v| v["digest"] == builtin["digest"]),
                format!("{proposal:?}"),
            );
            unchanged(
                &fx,
                &mut errors,
                &format!("positive {transport} check"),
                &tree,
                &catalog,
            );
            // Even if check failed on the pre-implementation baseline, try
            // apply with the independently calibrated builtin pin. A missing
            // proposal must not skip the new apply path or any negative case.
            if let Ok(proposal) = &proposal {
                apply["expected_new_digest"] = proposal["digest"].clone();
            }
            let reply = call(&fx, transport, Asserted::Operator, true, apply);
            record(
                &mut errors,
                &format!("positive {transport} pinned apply"),
                reply
                    .as_ref()
                    .is_ok_and(|v| v["install_id"] == installed["install_id"]),
                format!("{reply:?}"),
            );
            let shown = fx.op("app_workspace_show", json!({"install_id": id}));
            record(
                &mut errors,
                &format!("positive {transport} catalog show same id / builtin digest"),
                shown["install_id"] == installed["install_id"]
                    && shown["digest"] == builtin["digest"],
                format!("{shown}"),
            );
        }
        assert!(
            errors.is_empty(),
            "CAD-1244 acceptance failures:\n{}",
            errors.join("\n")
        );
    }
}
