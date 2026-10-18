//! CAD-1548 independent bad-case acceptance, authored against the ticket
//! and `/tmp/cad1548-contract.md`, not against an implementer's summary.
//! Implementers must not edit or weaken this file.
//!
//! One real daemon and one real board (`--features test-seam`). Caller
//! identity is the explicit seam only: this file never clears
//! `CADENCE_ALIAS`, never sets `CADENCE_TEST_AS`, and never uses a
//! daemon-child grant token.
//!
//! Proves, on the new Agents settings RPC and HTTP routes:
//! - an agent caller and an unproven caller cannot mutate them, including
//!   when the body forges operator attribution and when the HTTP request
//!   carries a fresh operator session;
//! - forged attribution fields are refused for the operator too, because
//!   authority is the connection, not a request field;
//! - a launch-policy-invalid save (Cursor-through-Pi plus an off-list Pi
//!   model granted `allow` and a role default) refuses before it changes
//!   the visible snapshot, `pm.yaml`, or the SQLite authority;
//! - profile ids `master` and path escapes, and a harness cli that is not a
//!   registered adapter, refuse the same way and create no escaped path
//!   or export;
//! - hand-written exports under `<pm>/.pi/agents` and `<pm>/.claude/agents`
//!   stay byte-for-byte, including after sync of a saved profile;
//! - the visible revision is `{model_defaults_revision}:{12 hex}` and a
//!   stale token refuses after a supported operator save.
//!
//! The visible result is the real GET snapshot. Opening this binary's
//! schema creates one `settings_authority` row (the v52 migration; the
//! owner witness requires that table at schema >= 52) and keeps the v51
//! artifact-fetch receipt table. Daemon startup then imports legacy
//! files before health. Once the daemon is serving, `legacy_imported`
//! is 1 and a profile that was already on disk is in that row.
//! `agent_settings_get`
//! is read-only: it does not import again and does not advance
//! `model_defaults.revision`. Later reads, sync, and preview leave the
//! row, `pm.yaml`, and `agents/` untouched. A refused or stale save
//! commits nothing. A committed save increments the revision by 1 and
//! writes profile bodies into the same row. `pi` NULL denies every Pi
//! launch. `AGENT.md` is not the authority.
//!
//! Sync refuses a symlinked export ancestor. A pre-existing
//! `<profile>.tmp` symlink is not a required refusal: Sync may refuse
//! it or write the managed export with its own temp. Either way the
//! outside sentinel and its directory stay byte-for-byte, and the
//! settings revision does not move. The real
//! importer refuses a symlinked profile directory and a symlinked
//! `agents` directory without setting `legacy_imported` or advancing
//! the revision. Caller `profile` / `profile_revision` /
//! `profile_digest` is refused on register and on set, and those
//! refusals write nothing. `agent_show` returns the stored profile
//! instructions only to the operator, that agent, or its PM, and only
//! when `include_profile_instructions` is boolean true; a normal show,
//! a non-profile row, and `GET /api/agents/<alias>?…` do not disclose
//! them. An isolated `Store::register_agent` inherits a Pi-kind role
//! default only for Pi when the declaration id collides with a native
//! provider. No second test:
//! all of this runs in the one daemon/HTTP check.
//!
//! Admitted run (manager; not executed by this author):
//! `cadence build-slot run test -- env CARGO_BUILD_JOBS=4 rtk proxy cargo test --locked --features test-seam --test cad1548_agents_settings_refusal_acceptance -- --test-threads 1`

#[cfg(all(feature = "test-seam", target_os = "linux"))]
mod acceptance {
    use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
    use cadence_agent::{client, daemon, issue, operator_auth};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::SystemTime;
    use std::time::{Duration, Instant};

    const GET: &str = "agent_settings_get";
    /// Dispatch name. The board's `POST /api/settings/agents` calls this.
    const SAVE: &str = "agent_settings_set";
    const CATALOG: &str = "agent_settings_catalog";
    const SYNC: &str = "agent_settings_sync";
    const PREVIEW: &str = "agent_settings_preview";

    const ROUTE: &str = "/api/settings/agents";
    const ROUTE_CATALOG: &str = "/api/settings/agents/catalog";
    const ROUTE_SYNC: &str = "/api/settings/agents/sync";
    const ROUTE_PREVIEW: &str = "/api/settings/agents/preview";

    const PROFILE: &str = "lane-pm";
    const LEGACY_PROFILE: &str = "legacy-lane";
    const LEGACY_TEXT: &str =
        "---\nrole: pm\nmodel: inherit\n---\ncadence-1548 legacy profile body\n";
    const LEGACY_NON_PROFILES: &[(&str, &[u8])] = &[
        (".gitkeep", b""),
        ("README.md", b"# Legacy profile definitions\n"),
        (".DS_Store", b"\0\0\0\x01Bud1"),
    ];
    const FORGED_PROFILE: &str = "forged-lane";
    const STALE_PROFILE: &str = "stale-lane";
    const ESCAPE_PROFILE: &str = "../outside";
    const CURSOR_THROUGH_PI: &str = "cursor/composer-2.5";
    const OFF_LIST: &str = "openrouter/not-on-allowlist";
    const ESCAPE_CLI: &str = "../../bin/sh";

    type Session = (String, String);

    /// `model_defaults` plus the v52 `settings_authority` row. Column
    /// set is that migration, not a document embedded in
    /// `model_defaults.document`.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Authority {
        defaults_revision: i64,
        defaults_document: String,
        pi: Option<String>,
        profiles: String,
        legacy_imported: i64,
    }

    struct Held {
        revision: Value,
        config: Value,
        pm_yaml: Vec<u8>,
        unmanaged: Vec<(PathBuf, Vec<u8>)>,
        sources: BTreeMap<String, (SystemTime, Vec<u8>)>,
        authority: Authority,
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
                let _ = thread.join();
            }
        }
    }

    impl Fx {
        fn start() -> Self {
            let root = tempfile::Builder::new().prefix("c1548-").tempdir().unwrap();
            let mut fx = Self {
                root,
                stop: Arc::new(AtomicBool::new(false)),
                threads: vec![],
                port: 0,
            };
            issue::Pm::init(&fx.pm()).unwrap();
            let legacy = fx.pm().join("agents").join(LEGACY_PROFILE).join("AGENT.md");
            std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            std::fs::write(&legacy, LEGACY_TEXT).unwrap();
            for (name, bytes) in LEGACY_NON_PROFILES {
                std::fs::write(fx.pm().join("agents").join(name), bytes).unwrap();
            }
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
            let state = self.state();
            scoped(who, || client::rpc(&state, method, params))
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.rpc(Asserted::Operator, method, params)
                .unwrap_or_else(|e| panic!("operator {method}: {e}"))
        }

        fn operator_session(&self) -> Session {
            operator_auth::ensure_secret(&self.state()).unwrap();
            let secret = operator_auth::read_secret(&self.state()).unwrap();
            let nonce = self.op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )["nonce"]
                .clone();
            let (status, reply, cookie) = self.http_post(
                &Asserted::Operator,
                "/api/session",
                &json!({"nonce": nonce}),
                None,
            );
            assert_eq!(status, 200, "operator session exchange failed: {reply}");
            let key = serde_json::from_str::<Value>(&reply).unwrap()["session_key"]
                .as_str()
                .unwrap()
                .to_string();
            (cookie.expect("session cookie"), key)
        }

        fn http_post(
            &self,
            who: &Asserted,
            route: &str,
            body: &Value,
            session: Option<&Session>,
        ) -> (u16, String, Option<String>) {
            self.exchange(who, "POST", route, Some(body), session)
        }

        fn http_get(
            &self,
            who: &Asserted,
            route: &str,
            session: Option<&Session>,
        ) -> (u16, String, Option<String>) {
            self.exchange(who, "GET", route, None, session)
        }

        fn exchange(
            &self,
            who: &Asserted,
            method: &str,
            route: &str,
            body: Option<&Value>,
            session: Option<&Session>,
        ) -> (u16, String, Option<String>) {
            let host = format!("cadence-{}.localhost:{}", self.port, self.port);
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let url = format!("http://127.0.0.1:{}{route}", self.port);
            let token = Seam::token_at(&self.state()).unwrap();
            let mut response = if method == "GET" {
                let mut req = agent
                    .get(&url)
                    .header("Host", &host)
                    .header("Origin", format!("http://{host}"))
                    .header("X-Cadence-Board", "1")
                    .header(AS_HEADER, who.as_str())
                    .header(TOKEN_HEADER, &token);
                if let Some((cookie, key)) = session {
                    req = req
                        .header("Cookie", cookie)
                        .header("X-Cadence-Session", key);
                }
                req.call().unwrap()
            } else {
                let mut req = agent
                    .post(&url)
                    .header("Host", &host)
                    .header("Origin", format!("http://{host}"))
                    .header("X-Cadence-Board", "1")
                    .header("Content-Type", "application/json")
                    .header(AS_HEADER, who.as_str())
                    .header(TOKEN_HEADER, &token);
                if let Some((cookie, key)) = session {
                    req = req
                        .header("Cookie", cookie)
                        .header("X-Cadence-Session", key);
                }
                req.send(body.expect("POST body").to_string()).unwrap()
            };
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

        /// Daemon snapshot and the board's GET, which must agree on the
        /// revision and the editable config.
        fn snapshot(&self) -> Value {
            let rpc = self.op(GET, json!({}));
            let session = self.operator_session();
            let (status, text, _) = self.http_get(&Asserted::Operator, ROUTE, Some(&session));
            assert_eq!(status, 200, "GET {ROUTE}: {text}");
            let http: Value = serde_json::from_str(&text).expect("settings snapshot json");
            assert_eq!(
                http["revision"], rpc["revision"],
                "HTTP revision diverged from {GET}"
            );
            assert_eq!(
                http["config"], rpc["config"],
                "HTTP config diverged from {GET}"
            );
            if rpc.get("read_only").is_some() {
                assert_eq!(
                    rpc["read_only"],
                    json!(false),
                    "operator fixture is read-only: {rpc}"
                );
            }
            assert_no_secret_fields(&rpc);
            assert!(
                rpc["config"].is_object(),
                "{GET} config is not an object: {rpc}"
            );
            rpc
        }

        fn live_agents(&self) -> usize {
            self.op("agent_list", json!({}))["agents"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0)
        }

        fn plant_unmanaged(&self) {
            let body = b"hand-written cadence-1548 export\nnot generated\n";
            for path in unmanaged_paths(&self.pm()) {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, body).unwrap();
            }
        }

        fn hold(&self, snap: &Value) -> Held {
            Held {
                revision: snap["revision"].clone(),
                config: snap["config"].clone(),
                pm_yaml: read_bytes(&self.pm().join("pm.yaml")),
                unmanaged: unmanaged_paths(&self.pm())
                    .into_iter()
                    .map(|path| {
                        let bytes = std::fs::read(&path).unwrap();
                        (path, bytes)
                    })
                    .collect(),
                sources: source_files(&self.pm()),
                authority: authority(&self.state()),
            }
        }
    }

    fn authority(state: &Path) -> Authority {
        let conn = rusqlite::Connection::open_with_flags(
            state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open state database");
        conn.busy_timeout(Duration::from_secs(2)).unwrap();
        let (defaults_revision, defaults_document): (i64, String) = conn
            .query_row(
                "SELECT revision, document FROM model_defaults WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("model_defaults authority row");
        let schema: i64 = conn
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .expect("schema_version");
        assert_eq!(
            schema,
            cadence_agent::rollout::SCHEMA_VERSION,
            "opened database schema {schema} is not this binary's schema {}",
            cadence_agent::rollout::SCHEMA_VERSION
        );
        let fetch_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='app_run_artifact_fetches'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            fetch_tables, 1,
            "v51 artifact-fetch receipt table is missing after open"
        );
        let defaults_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM model_defaults", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            defaults_rows, 1,
            "model_defaults is not the single authority row"
        );
        let table: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='settings_authority'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            table, 1,
            "settings_authority is created by the v52 migration, not by a settings call"
        );
        let authority_rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM settings_authority", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            authority_rows, 1,
            "settings_authority must be the single id=1 row"
        );
        let (pi, profiles, legacy_imported): (Option<String>, String, i64) = conn
            .query_row(
                "SELECT pi, profiles, legacy_imported FROM settings_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("settings_authority row");
        assert!(
            matches!(legacy_imported, 0 | 1),
            "legacy_imported is not 0/1: {legacy_imported}"
        );
        let document: Value =
            serde_json::from_str(&defaults_document).expect("model_defaults document is json");
        assert!(
            document.get("authority").is_none(),
            "model_defaults still embeds settings authority: {document}"
        );
        let _ = assert_profile_rows(&profiles);
        Authority {
            defaults_revision,
            defaults_document,
            pi,
            profiles,
            legacy_imported,
        }
    }

    fn assert_profile_rows(profiles: &str) -> Vec<Value> {
        let rows: Vec<Value> =
            serde_json::from_str(profiles).expect("settings_authority.profiles is a JSON array");
        for row in &rows {
            assert!(
                row.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty()),
                "profile row has no id: {row}"
            );
            assert!(
                row.get("text").and_then(Value::as_str).is_some(),
                "profile row has no text: {row}"
            );
        }
        rows
    }

    fn authority_profile_text(authority: &Authority, id: &str) -> Option<String> {
        assert_profile_rows(&authority.profiles)
            .into_iter()
            .find(|row| row["id"].as_str() == Some(id))
            .and_then(|row| row.get("text").and_then(Value::as_str).map(str::to_string))
    }

    fn authority_profile_ids(authority: &Authority) -> Vec<String> {
        assert_profile_rows(&authority.profiles)
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
            .collect()
    }

    fn unmanaged_paths(pm: &Path) -> Vec<PathBuf> {
        vec![
            pm.join(".pi").join("agents").join(format!("{PROFILE}.md")),
            pm.join(".claude")
                .join("agents")
                .join(format!("{PROFILE}.md")),
        ]
    }

    fn read_bytes(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_default()
    }

    /// `pm.yaml` and `agents/`, the files a later settings read must not touch.
    fn source_files(pm: &Path) -> BTreeMap<String, (SystemTime, Vec<u8>)> {
        let mut out = BTreeMap::new();
        let yaml = pm.join("pm.yaml");
        if yaml.exists() {
            record_file(&yaml, "pm.yaml", &mut out);
        }
        let agents = pm.join("agents");
        if agents.is_dir() {
            walk_files(&agents, "agents", &mut out);
        }
        out
    }

    fn walk_files(dir: &Path, rel: &str, out: &mut BTreeMap<String, (SystemTime, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            let rel = format!("{rel}/{name}");
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                let modified = meta.modified().unwrap();
                let target = std::fs::read_link(&path).unwrap();
                out.insert(
                    rel,
                    (modified, target.to_string_lossy().into_owned().into_bytes()),
                );
            } else if meta.is_dir() {
                walk_files(&path, &rel, out);
            } else {
                record_file(&path, &rel, out);
            }
        }
    }

    fn record_file(path: &Path, rel: &str, out: &mut BTreeMap<String, (SystemTime, Vec<u8>)>) {
        let meta = std::fs::metadata(path).unwrap();
        out.insert(
            rel.to_string(),
            (meta.modified().unwrap(), std::fs::read(path).unwrap()),
        );
    }

    fn revision_token(value: &Value) -> bool {
        let Some(token) = value.as_str() else {
            return false;
        };
        let Some((rev, hash)) = token.split_once(':') else {
            return false;
        };
        !rev.is_empty()
            && rev.chars().all(|c| c.is_ascii_digit())
            && hash.len() == 12
            && hash.chars().all(|c| c.is_ascii_hexdigit())
    }

    fn assert_held(fx: &Fx, held: &Held) {
        let snap = fx.snapshot();
        assert!(
            revision_token(&snap["revision"]),
            "revision shape: {}",
            snap["revision"]
        );
        assert_eq!(snap["revision"], held.revision, "revision moved: {snap}");
        assert_eq!(
            snap["config"], held.config,
            "visible defaults moved: {snap}"
        );
        assert_eq!(
            read_bytes(&fx.pm().join("pm.yaml")),
            held.pm_yaml,
            "pm.yaml changed after a refusal"
        );
        for (path, bytes) in &held.unmanaged {
            assert_eq!(
                std::fs::read(path).unwrap_or_default(),
                *bytes,
                "unmanaged export changed: {}",
                path.display()
            );
        }
        assert_eq!(
            source_files(&fx.pm()),
            held.sources,
            "pm.yaml or agents/ changed after import"
        );
        assert_eq!(
            authority(&fx.state()),
            held.authority,
            "model_defaults or settings_authority changed after a refusal"
        );
        assert_eq!(fx.live_agents(), 0, "a settings call launched an agent");
    }

    fn assert_no_escape(fx: &Fx, absolute: &Path) {
        assert!(
            !absolute.exists(),
            "escaped path created: {}",
            absolute.display()
        );
        assert!(
            !fx.pm().join("outside").exists(),
            "profile id escaped the agents directory"
        );
        assert!(!fx.pm().join("agents").join(ESCAPE_PROFILE).exists());
    }

    const ROLES: &[&str] = &[
        "pm",
        "research",
        "architect",
        "dev",
        "qa",
        "devops",
        "worker",
    ];

    /// Round-trip shape `parse_overlay` requires, filled from whatever the
    /// snapshot already returned. Empty role defaults are not launch grants.
    fn ensure_shape(config: &mut Value) {
        let obj = config.as_object_mut().expect("config object");
        for key in ["harnesses", "providers", "models", "profiles", "pins"] {
            if !obj.get(key).is_some_and(Value::is_array) {
                obj.insert(key.to_string(), json!([]));
            }
        }
        if !obj.get("scope").is_some_and(Value::is_object) {
            obj.insert("scope".to_string(), json!({}));
        }
        if !obj.get("roleDefaults").is_some_and(Value::is_object) {
            obj.insert("roleDefaults".to_string(), json!({}));
        }
        let models: Vec<String> = obj["models"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["id"].as_str().map(str::to_string))
            .collect();
        let scope = obj.get_mut("scope").unwrap().as_object_mut().unwrap();
        for id in models {
            scope.entry(id).or_insert_with(|| json!("off"));
        }
        let defaults = obj
            .get_mut("roleDefaults")
            .unwrap()
            .as_object_mut()
            .unwrap();
        for role in ROLES {
            defaults.entry(*role).or_insert_with(|| json!(""));
        }
    }

    fn save_body(revision: &Value, config: Value) -> Value {
        json!({"expected_revision": revision, "config": config})
    }

    fn forge(mut body: Value) -> Value {
        let obj = body.as_object_mut().expect("object body");
        obj.insert("by".into(), json!("forged-operator"));
        obj.insert("operator".into(), json!("forged-operator"));
        obj.insert("actor".into(), json!("forged-operator"));
        obj.insert("attribution".into(), json!("operator"));
        obj.insert("as".into(), json!("operator"));
        obj.insert("sub".into(), json!("operator"));
        body
    }

    fn profiles(config: &mut Value) -> &mut Vec<Value> {
        config
            .as_object_mut()
            .expect("config")
            .entry("profiles")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .expect("profiles")
    }

    fn profile(id: &str, role: &str) -> Value {
        json!({"id": id, "role": role, "model": "inherit", "job": "Acceptance profile."})
    }

    fn supported_config(config: &Value) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        let exists = config["harnesses"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"].as_str() == Some("pi"));
        if !exists {
            config["harnesses"].as_array_mut().unwrap().push(json!({
                "id": "pi",
                "label": "Pi",
                "cli": "pi",
                "note": "Registered adapter declaration."
            }));
        }
        if !has_profile(&config, PROFILE) {
            profiles(&mut config).push(profile(PROFILE, "pm"));
        }
        config
    }

    /// Structurally complete, launch-illegal: a Pi-extension provider with
    /// Cursor-through-Pi and an off-list model both scoped `allow` and
    /// installed as role defaults. Empty pin so the refusal is not a pin
    /// grammar error. Saving this must not write the allowlist.
    fn policy_config(config: &Value) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        let exists = config["providers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"].as_str() == Some("pi"));
        if !exists {
            config["providers"].as_array_mut().unwrap().push(json!({
                "id": "pi",
                "label": "Pi",
                "kind": "Pi extension",
                "pin": "",
                "gate": true,
                "paid": false,
                "note": ""
            }));
        }
        config["models"].as_array_mut().unwrap().extend([
            json!({
                "id": CURSOR_THROUGH_PI,
                "provider": "pi",
                "ctx": "",
                "out": "",
                "thinking": null,
                "images": null
            }),
            json!({
                "id": OFF_LIST,
                "provider": "pi",
                "ctx": "",
                "out": "",
                "thinking": null,
                "images": null
            }),
        ]);
        ensure_shape(&mut config);
        let scope = config["scope"].as_object_mut().unwrap();
        scope.insert(CURSOR_THROUGH_PI.into(), json!("allow"));
        scope.insert(OFF_LIST.into(), json!("allow"));
        let defaults = config["roleDefaults"].as_object_mut().unwrap();
        defaults.insert("worker".into(), json!(CURSOR_THROUGH_PI));
        defaults.insert("dev".into(), json!(OFF_LIST));
        config
    }

    fn escape_config(config: &Value, absolute: &Path) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        config
            .as_object_mut()
            .unwrap()
            .entry("harnesses")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id": "escape",
                "label": "Escape",
                "cli": ESCAPE_CLI,
                "note": "Not a registered adapter."
            }));
        let rows = profiles(&mut config);
        rows.push(profile(ESCAPE_PROFILE, "dev"));
        rows.push(profile("..", "dev"));
        rows.push(profile(&absolute.to_string_lossy(), "dev"));
        config
    }

    fn master_config(config: &Value) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        profiles(&mut config).push(profile("master", "pm"));
        config
    }

    fn stale_config(config: &Value) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        profiles(&mut config).push(profile(STALE_PROFILE, "qa"));
        config
    }

    fn forged_config(config: &Value) -> Value {
        let mut config = config.clone();
        ensure_shape(&mut config);
        profiles(&mut config).push(profile(FORGED_PROFILE, "worker"));
        config
    }

    fn has_profile(config: &Value, id: &str) -> bool {
        config["profiles"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["id"].as_str() == Some(id)))
    }

    fn is_operator_gate(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method") && lower.contains("operator")
    }

    fn is_forged_field(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && [
                "not accepted",
                "connection-bound",
                "unknown field",
                "unexpected",
                "attribution",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
    }

    fn is_launch_policy(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && [
                "allowlist",
                "allow list",
                "off-list",
                "off list",
                "unsafe argv",
                "transport",
                "agentic",
                "cad-603",
                "cad-602",
                "cad-559",
                "native cursor",
                "through-pi",
                "through pi",
                "cursor-through",
                "launch",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
    }

    fn is_path_refusal(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && [
                "path",
                "escape",
                "slug",
                "..",
                "directory",
                "outside",
                "adapter",
                "executable",
                "registry",
                "launcher",
                "not supported",
            ]
            .iter()
            .any(|needle| lower.contains(needle))
    }

    fn is_revision_conflict(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && (lower.contains("revision") || lower.contains("conflict") || lower.contains("stale"))
    }

    fn assert_no_secret_fields(value: &Value) {
        const BANNED: &[&str] = &[
            "api_key",
            "apikey",
            "secret",
            "password",
            "authorization",
            "access_token",
            "refresh_token",
            "private_key",
        ];
        fn walk(value: &Value) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        let lower = key.to_ascii_lowercase();
                        assert!(
                            !BANNED.iter().any(|ban| lower == *ban),
                            "settings payload exposed secret field {key}"
                        );
                        walk(child);
                    }
                }
                Value::Array(items) => items.iter().for_each(walk),
                _ => {}
            }
        }
        walk(value);
    }

    fn parse_json(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|_| json!({"error": text}))
    }

    fn is_provenance_refusal(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && (lower.contains("provenance") || lower.contains("profile identity is fixed"))
    }

    fn open_state(state: &Path) -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_with_flags(
            state.join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .expect("open state database");
        conn.busy_timeout(Duration::from_secs(2)).unwrap();
        conn
    }

    fn agent_aliases(state: &Path) -> Vec<String> {
        let conn = open_state(state);
        let mut stmt = conn
            .prepare("SELECT alias FROM agents ORDER BY alias")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    fn stored_agent(state: &Path, alias: &str) -> Option<(Option<String>, Value)> {
        let conn = open_state(state);
        conn.query_row(
            "SELECT instructions, COALESCE(params, '{}') FROM agents WHERE alias=?1",
            [alias],
            |row| {
                let instructions: Option<String> = row.get(0)?;
                let params: String = row.get(1)?;
                Ok((instructions, params))
            },
        )
        .ok()
        .map(|(instructions, params)| {
            (
                instructions,
                serde_json::from_str(&params).expect("agent params json"),
            )
        })
    }

    fn is_link_refusal(text: &str) -> bool {
        let lower = text.to_lowercase();
        !lower.contains("unknown method")
            && (lower.contains("symlink")
                || lower.contains("symbolic")
                || lower.contains("eloop")
                || lower.contains("nofollow")
                || lower.contains("no-follow")
                || lower.contains("ancestor")
                || lower.contains("through link"))
    }

    fn remove_path(path: &Path) {
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return;
        };
        if meta.file_type().is_symlink() || !meta.is_dir() {
            std::fs::remove_file(path).unwrap();
        } else {
            std::fs::remove_dir_all(path).unwrap();
        }
    }

    fn dir_names(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Ancestor symlink must refuse. A predictable `<profile>.tmp` symlink
    /// must not clobber the outside sentinel: Sync may refuse that call or
    /// ignore the unused temp and write the managed export.
    fn refuse_export_symlinks(fx: &Fx, revision: &Value) {
        let outside = fx.root.path().join("outside-sentinel");
        std::fs::create_dir_all(&outside).unwrap();
        let sentinel = outside.join("SENTINEL");
        let marker = b"cadence-1548 outside sentinel\n";
        std::fs::write(&sentinel, marker).unwrap();
        let before = authority(&fx.state());
        let body = json!({"expected_revision": revision, "profile": LEGACY_PROFILE});
        let pi_md = fx
            .pm()
            .join(".pi")
            .join("agents")
            .join(format!("{LEGACY_PROFILE}.md"));
        assert!(pi_md.is_file(), "saved sync did not write the pi export");

        let claude = fx.pm().join(".claude").join("agents");
        remove_path(&claude);
        std::os::unix::fs::symlink(&outside, &claude).unwrap();
        let err = fx
            .rpc(Asserted::Operator, SYNC, body.clone())
            .expect_err("sync followed a symlinked export ancestor");
        assert!(
            is_link_refusal(&err.to_string()),
            "ancestor sync was not a symlink refusal: {err}"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), marker);
        assert_eq!(dir_names(&outside), vec!["SENTINEL".to_string()]);
        assert_eq!(
            authority(&fx.state()),
            before,
            "ancestor sync committed settings"
        );
        assert_eq!(fx.snapshot()["revision"], *revision);

        remove_path(&claude);
        std::fs::create_dir_all(&claude).unwrap();
        let hand = b"hand-written cadence-1548 export\n";
        let claude_md = claude.join(format!("{LEGACY_PROFILE}.md"));
        std::fs::write(&claude_md, hand).unwrap();
        remove_path(&pi_md);
        let tmp = pi_md.with_extension("tmp");
        remove_path(&tmp);
        std::os::unix::fs::symlink(&sentinel, &tmp).unwrap();
        let outcome = fx.rpc(Asserted::Operator, SYNC, body);
        assert_eq!(
            std::fs::read(&sentinel).unwrap(),
            marker,
            "temp symlink clobbered the outside sentinel"
        );
        assert_eq!(
            dir_names(&outside),
            vec!["SENTINEL".to_string()],
            "temp sync changed the outside directory"
        );
        assert_eq!(
            std::fs::read(&claude_md).unwrap(),
            hand,
            "temp sync overwrote the hand-written export"
        );
        assert_eq!(
            authority(&fx.state()),
            before,
            "temp sync committed settings"
        );
        assert_eq!(fx.snapshot()["revision"], *revision);
        match &outcome {
            Err(_) => {
                if symlink_exists(&pi_md) {
                    assert_managed_export(&pi_md, &sentinel);
                }
            }
            Ok(_) => assert_managed_export(&pi_md, &sentinel),
        }
        assert_eq!(fx.live_agents(), 0, "sync launched an agent");
    }

    fn symlink_exists(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok()
    }

    /// Managed export for the imported legacy profile: a regular file, not
    /// the outside sentinel, carrying that profile's body and the
    /// generated-file ownership marker.
    fn assert_managed_export(path: &Path, sentinel: &Path) {
        let meta = std::fs::symlink_metadata(path)
            .unwrap_or_else(|err| panic!("managed export {}: {err}", path.display()));
        assert!(
            meta.file_type().is_file(),
            "managed export is not a regular file: {}",
            path.display()
        );
        let sent = std::fs::symlink_metadata(sentinel).unwrap();
        assert_ne!(
            (meta.dev(), meta.ino()),
            (sent.dev(), sent.ino()),
            "managed export is the outside sentinel"
        );
        let text = std::fs::read_to_string(path).unwrap();
        assert!(
            text.contains("cadence-1548 legacy profile body"),
            "export missing profile body: {text}"
        );
        assert!(
            text.contains(LEGACY_PROFILE),
            "export missing profile id: {text}"
        );
        assert!(
            text.contains("cadence-agent-profile"),
            "export missing ownership marker: {text}"
        );
        assert!(
            !text.contains("outside sentinel"),
            "export contains the outside sentinel: {text}"
        );
    }

    /// Profile instructions and the three daemon-owned provenance keys.
    /// Cloud launch validation may admit the keys; register and set still
    /// refuse a caller value and leave the database unchanged. The show
    /// opt-in returns the stored job only to the operator, the agent, or
    /// its PM. Inbox rows stand in for a launch so no provider is started.
    fn prove_profile_launch_provenance(fx: &Fx, revision: &Value) {
        let job = "cadence-1548 legacy profile body";
        let private = "cadence-1548 private mailbox text";
        let before = authority(&fx.state());
        assert!(
            agent_aliases(&fx.state()).is_empty(),
            "provenance fixture started with agents"
        );
        let cwd = fx.pm().to_string_lossy().into_owned();

        for key in ["profile", "profile_revision", "profile_digest"] {
            let forged = json!({ key: "forged" });
            assert!(
                cadence_agent::adapter::registry::validate_launch_params("devin", "cloud", &forged)
                    .is_ok(),
                "devin cloud rejected daemon-owned metadata key {key}"
            );
            let alias = format!("c1548-spoof-{key}");
            let err = fx
                .rpc(
                    Asserted::Operator,
                    "agent_register",
                    json!({
                        "alias": alias,
                        "provider": "devin",
                        "endpoint_kind": "cloud",
                        "cwd": cwd,
                        "params": forged.to_string(),
                    }),
                )
                .expect_err("caller profile metadata was stored");
            assert!(
                is_provenance_refusal(&err.to_string()),
                "register spoof of {key} was not a provenance refusal: {err}"
            );
            assert!(
                !err.to_string().contains(job),
                "register spoof disclosed the profile job: {err}"
            );
            assert!(
                stored_agent(&fx.state(), &alias).is_none(),
                "spoofed register wrote {alias}"
            );
        }

        let forged_digest = json!({ "profile_digest": "deadbeef" });
        assert!(
            cadence_agent::adapter::registry::validate_launch_params(
                "devin",
                "cloud",
                &forged_digest
            )
            .is_ok(),
            "devin cloud rejected profile_digest"
        );
        let err = fx
            .rpc(
                Asserted::Operator,
                "agent_register",
                json!({
                    "alias": "c1548-spoof-with-profile",
                    "provider": "devin",
                    "endpoint_kind": "cloud",
                    "cwd": cwd,
                    "profile": LEGACY_PROFILE,
                    "params": forged_digest.to_string(),
                }),
            )
            .expect_err("profile launch accepted caller provenance");
        assert!(
            is_provenance_refusal(&err.to_string()),
            "profile plus caller digest was not refused: {err}"
        );
        assert!(stored_agent(&fx.state(), "c1548-spoof-with-profile").is_none());
        assert_eq!(
            authority(&fx.state()),
            before,
            "spoof register wrote settings"
        );
        assert_eq!(fx.snapshot()["revision"], *revision);
        assert_eq!(fx.live_agents(), 0, "spoof register launched an agent");

        fx.op(
            "agent_register",
            json!({
                "alias": "c1548-parent",
                "provider": "inbox",
                "endpoint_kind": "inbox",
                "role": "pm",
                "cwd": cwd,
            }),
        );
        fx.op(
            "agent_register",
            json!({
                "alias": "c1548-profile",
                "provider": "inbox",
                "endpoint_kind": "inbox",
                "role": "worker",
                "cwd": cwd,
                "profile": LEGACY_PROFILE,
                "params": json!({"upstream": "c1548-parent"}).to_string(),
            }),
        );
        fx.op(
            "agent_register",
            json!({
                "alias": "c1548-stranger",
                "provider": "inbox",
                "endpoint_kind": "inbox",
                "role": "worker",
                "cwd": cwd,
                "instructions": private,
            }),
        );
        assert_eq!(
            authority(&fx.state()),
            before,
            "inbox register wrote settings"
        );
        let (stored_job, stored_params) =
            stored_agent(&fx.state(), "c1548-profile").expect("profile mailbox missing");
        let stored_job = stored_job.expect("profile mailbox stored no instructions");
        assert!(
            stored_job.contains(job),
            "stored instructions are not the profile job: {stored_job}"
        );
        assert_eq!(stored_params["profile"], json!(LEGACY_PROFILE));
        assert_eq!(stored_params["upstream"], json!("c1548-parent"));
        assert!(
            stored_params["profile_revision"].is_number(),
            "daemon profile_revision was not numeric: {}",
            stored_params["profile_revision"]
        );
        assert!(
            stored_params["profile_digest"]
                .as_str()
                .is_some_and(|digest| !digest.is_empty() && digest != "deadbeef"),
            "daemon profile_digest was caller-controlled: {}",
            stored_params["profile_digest"]
        );
        let (secret, _) =
            stored_agent(&fx.state(), "c1548-stranger").expect("stranger mailbox missing");
        assert_eq!(secret.as_deref(), Some(private));

        let quiet = fx.op("agent_show", json!({ "alias": "c1548-profile" }));
        assert!(
            quiet["agent"].get("instructions").is_none(),
            "normal show disclosed instructions: {quiet}"
        );
        assert!(!quiet.to_string().contains(job));
        let flagged_off = fx.op(
            "agent_show",
            json!({
                "alias": "c1548-profile",
                "include_profile_instructions": false,
            }),
        );
        assert!(flagged_off["agent"].get("instructions").is_none());
        let bad_flag = fx
            .rpc(
                Asserted::Operator,
                "agent_show",
                json!({
                    "alias": "c1548-profile",
                    "include_profile_instructions": "true",
                }),
            )
            .expect_err("a string opt-in was accepted");
        assert!(
            bad_flag.to_string().to_lowercase().contains("boolean"),
            "string opt-in was not a boolean refusal: {bad_flag}"
        );
        assert!(!bad_flag.to_string().contains(job));

        let shown = fx.op(
            "agent_show",
            json!({
                "alias": "c1548-profile",
                "include_profile_instructions": true,
            }),
        );
        assert_eq!(
            shown["agent"]["instructions"].as_str(),
            Some(stored_job.as_str())
        );
        assert_eq!(shown["agent"]["state"], json!("idle"));
        assert_eq!(shown["agent"]["provider"], json!("inbox"));
        assert_eq!(
            shown["agent"]["params"]["profile"],
            stored_params["profile"]
        );
        assert_eq!(
            shown["agent"]["params"]["profile_revision"],
            stored_params["profile_revision"]
        );
        assert_eq!(
            shown["agent"]["params"]["profile_digest"],
            stored_params["profile_digest"]
        );
        for who in [
            Asserted::Operator,
            Asserted::Agent("c1548-profile".to_string()),
            Asserted::Agent("c1548-parent".to_string()),
        ] {
            let again = fx
                .rpc(
                    who.clone(),
                    "agent_show",
                    json!({
                        "alias": "c1548-profile",
                        "include_profile_instructions": true,
                    }),
                )
                .unwrap_or_else(|err| panic!("allowed snapshot for {who:?}: {err}"));
            assert_eq!(
                again["agent"]["instructions"].as_str(),
                Some(stored_job.as_str()),
                "snapshot for {who:?} was not the stored job"
            );
            assert_eq!(
                again["agent"]["params"]["profile_digest"],
                stored_params["profile_digest"]
            );
        }
        for who in [
            Asserted::Agent("c1548-stranger".to_string()),
            Asserted::Unproven,
        ] {
            let err = fx
                .rpc(
                    who.clone(),
                    "agent_show",
                    json!({
                        "alias": "c1548-profile",
                        "include_profile_instructions": true,
                    }),
                )
                .expect_err("disallowed caller read profile instructions");
            assert!(
                !err.to_string().contains(job) && !err.to_string().contains(private),
                "refused snapshot leaked instructions to {who:?}: {err}"
            );
        }
        let (still_job, still_params) =
            stored_agent(&fx.state(), "c1548-profile").expect("profile row disappeared");
        assert_eq!(still_job.as_deref(), Some(stored_job.as_str()));
        assert_eq!(still_params, stored_params, "show wrote agent params");

        let stranger_show = fx.op(
            "agent_show",
            json!({
                "alias": "c1548-stranger",
                "include_profile_instructions": true,
            }),
        );
        assert!(
            stranger_show["agent"].get("instructions").is_none(),
            "non-profile instructions disclosed: {stranger_show}"
        );
        assert!(!stranger_show.to_string().contains(private));

        let session = fx.operator_session();
        let (status, reply, _) = fx.http_get(
            &Asserted::Operator,
            "/api/agents/c1548-profile?include_profile_instructions=true",
            Some(&session),
        );
        assert_eq!(status, 200, "board agent show: {reply}");
        let http = parse_json(&reply);
        assert!(
            http["agent"].get("instructions").is_none(),
            "HTTP query opted into instructions: {reply}"
        );
        assert!(!reply.contains(job), "HTTP show leaked the profile job");

        for key in ["profile", "profile_revision", "profile_digest"] {
            let err = fx
                .rpc(
                    Asserted::Operator,
                    "agent_set",
                    json!({
                        "alias": "c1548-profile",
                        "patch": { key: "forged", "inbox_warn_unread": 9 },
                    }),
                )
                .expect_err("provenance patch was applied");
            assert!(
                is_provenance_refusal(&err.to_string()),
                "set {key} was not a provenance refusal: {err}"
            );
            assert!(!err.to_string().contains(job));
            let (_, params) =
                stored_agent(&fx.state(), "c1548-profile").expect("profile row disappeared");
            assert_eq!(params, stored_params, "refused set wrote params");
        }
        let err = fx
            .rpc(
                Asserted::Agent("c1548-stranger".to_string()),
                "agent_set",
                json!({
                    "alias": "c1548-profile",
                    "patch": { "profile": "forged-lane" },
                }),
            )
            .expect_err("stranger patched provenance");
        assert!(!err.to_string().contains(job));
        let (_, params) = stored_agent(&fx.state(), "c1548-profile").unwrap();
        assert_eq!(params, stored_params, "stranger set wrote params");
        assert_eq!(
            authority(&fx.state()),
            before,
            "profile reads wrote settings"
        );
        assert_eq!(fx.snapshot()["revision"], *revision);

        for alias in ["c1548-profile", "c1548-stranger", "c1548-parent"] {
            fx.op("agent_remove", json!({ "alias": alias }));
        }
        assert!(
            agent_aliases(&fx.state()).is_empty(),
            "removed mailboxes left rows"
        );
        assert_eq!(fx.live_agents(), 0, "provenance check left an agent");
        assert_eq!(authority(&fx.state()), before);
        assert_eq!(fx.snapshot()["revision"], *revision);
    }

    /// Direct `Store::import_legacy_settings`. A profile-directory symlink
    /// and an `agents` directory symlink both refuse, and neither sets
    /// `legacy_imported` nor advances `model_defaults.revision`.
    fn refuse_symlinked_import() {
        let root = tempfile::Builder::new()
            .prefix("c1548-import-")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let pm = root.path().join("pm");
        let outside = root.path().join("outside");
        let nest = outside.join("nest");
        std::fs::create_dir_all(&nest).unwrap();
        let external = "outside cadence-1548 profile\n";
        let kept = "kept cadence-1548 profile\n";
        std::fs::write(nest.join("AGENT.md"), external).unwrap();
        let sentinel = outside.join("SENTINEL");
        let marker = b"cadence-1548 import sentinel\n";
        std::fs::write(&sentinel, marker).unwrap();
        std::fs::create_dir_all(pm.join("agents").join("kept")).unwrap();
        std::fs::write(pm.join("agents").join("kept").join("AGENT.md"), kept).unwrap();
        std::os::unix::fs::symlink(&nest, pm.join("agents").join("planted")).unwrap();

        let db = state.join("cadence.sqlite3");
        let before_rev = {
            let store = cadence_agent::store::Store::open(&db).unwrap();
            let before = store.settings().unwrap();
            let rev = before.revision;
            assert!(!before.legacy_imported);
            assert!(before.doc.pi.is_none());
            assert!(before.doc.profiles.is_empty());
            let err = store
                .import_legacy_settings(&pm)
                .expect_err("import followed agents/planted symlink");
            assert!(
                is_link_refusal(&err.to_string()),
                "profile directory symlink was not refused: {err}"
            );
            let after = store.settings().unwrap();
            assert!(!after.legacy_imported, "refused import set legacy_imported");
            assert_eq!(after.revision, before.revision);
            assert!(
                after.doc.profiles.is_empty(),
                "refused import stored a profile"
            );
            assert!(after.doc.pi.is_none());
            rev
        };
        assert_eq!(std::fs::read(&sentinel).unwrap(), marker);
        assert_eq!(
            std::fs::read(nest.join("AGENT.md")).unwrap(),
            external.as_bytes()
        );
        let row = authority(&state);
        assert_eq!(row.legacy_imported, 0);
        assert_eq!(row.defaults_revision, before_rev);
        assert!(!row.profiles.contains("outside cadence-1548"));
        assert!(!row.profiles.contains("kept cadence-1548"));

        let root_body = "agents-root cadence-1548 profile\n";
        let linked = root.path().join("agents-link");
        std::fs::create_dir_all(linked.join("planted")).unwrap();
        std::fs::write(linked.join("planted").join("AGENT.md"), root_body).unwrap();
        remove_path(&pm.join("agents"));
        std::os::unix::fs::symlink(&linked, pm.join("agents")).unwrap();
        {
            let store = cadence_agent::store::Store::open(&db).unwrap();
            let err = store
                .import_legacy_settings(&pm)
                .expect_err("import followed a symlinked agents directory");
            assert!(
                is_link_refusal(&err.to_string()),
                "agents directory symlink was not refused: {err}"
            );
            let after = store.settings().unwrap();
            assert!(
                !after.legacy_imported,
                "second import committed legacy_imported"
            );
            assert_eq!(after.revision, before_rev);
            assert!(
                after
                    .doc
                    .profiles
                    .iter()
                    .all(|row| !row.text.contains("cadence-1548")),
                "symlinked agents directory was imported"
            );
            assert!(after.doc.pi.is_none());
        }
        assert_eq!(std::fs::read(&sentinel).unwrap(), marker);
        assert_eq!(
            std::fs::read(linked.join("planted").join("AGENT.md")).unwrap(),
            root_body.as_bytes()
        );
        let row = authority(&state);
        assert_eq!(row.legacy_imported, 0);
        assert_eq!(row.defaults_revision, before_rev);
        assert!(row.pi.is_none());
        assert!(!row.profiles.contains(root_body.trim()));
    }

    /// Pi-kind declaration id `claude` with `roleDefaults.dev` set to
    /// `fixture/pi-only`. `Store::register_agent` is the consumer: Pi
    /// inherits that id and Claude does not. Isolated store, no actor.
    fn pi_kind_role_default_reaches_pi_only() {
        const MODEL: &str = "fixture/pi-only";
        let root = tempfile::Builder::new()
            .prefix("c1548-route-")
            .tempdir()
            .unwrap();
        let pm_dir = root.path().join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        std::fs::write(pm_dir.join("pm.yaml"), "schema: 1\n").unwrap();
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let store = cadence_agent::store::Store::open(&state.join("cadence.sqlite3")).unwrap();
        store.import_legacy_settings(&pm_dir).unwrap();
        let pm = cadence_agent::issue::Pm::at(&pm_dir).unwrap();
        let snap = cadence_agent::agent_settings::snapshot(&pm.dir, &store, &[]).unwrap();
        let mut config = json!({
            "harnesses": [{
                "id": "pi",
                "label": "Pi",
                "cli": "pi",
                "note": "",
            }],
            "providers": [{
                "id": "claude",
                "label": "Pi extension",
                "kind": "Pi extension",
                "pin": "",
                "gate": true,
                "paid": false,
                "note": "",
            }],
            "models": [{
                "id": MODEL,
                "provider": "claude",
                "ctx": "",
                "out": "",
                "thinking": null,
                "images": null,
            }],
            "scope": {},
            "roleDefaults": {},
            "profiles": [],
            "pins": [],
        });
        ensure_shape(&mut config);
        config["scope"]
            .as_object_mut()
            .unwrap()
            .insert(MODEL.into(), json!("allow"));
        config["roleDefaults"]
            .as_object_mut()
            .unwrap()
            .insert("dev".into(), json!(MODEL));
        let token = snap["revision"].as_str().expect("revision token");
        cadence_agent::agent_settings::save(&pm, &state, &store, &[], token, &config)
            .unwrap_or_else(|err| panic!("Pi-kind role default was not saved: {err}"));
        let cwd = pm_dir.to_str().expect("utf8 pm path");
        for (alias, provider) in [("c1548-pi", "pi"), ("c1548-claude", "claude")] {
            store
                .register_agent(&cadence_agent::store::NewAgent {
                    alias,
                    provider,
                    endpoint_kind: "managed",
                    role: "worker",
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: Some("dev"),
                    model_policy: None,
                })
                .unwrap_or_else(|err| panic!("register {provider}: {err}"));
        }
        let pi = store.agent("c1548-pi").unwrap().model_selection;
        let native = store.agent("c1548-claude").unwrap().model_selection;
        assert_eq!(
            pi.as_ref().and_then(|row| row["model"].as_str()),
            Some(MODEL),
            "Pi register did not inherit the role default: {pi:?}"
        );
        assert_ne!(
            native.as_ref().and_then(|row| row["model"].as_str()),
            Some(MODEL),
            "Claude register inherited the Pi selector: {native:?}"
        );
    }

    /// Canonical policy/profile authority is omitted from portable copies;
    /// an unknown authority column refuses before package publication.
    fn portable_settings_authority_is_omitted_and_unknown_columns_refuse() {
        use cadence_agent::backup::portable::{
            export_portable, restore_inactive, PortableExportOptions, PortableManifest,
            PortableRestoreOptions, Scope,
        };

        let root = tempfile::Builder::new()
            .prefix("c1548-portable-")
            .tempdir()
            .unwrap();
        let pm = root.path().join("pm");
        let profile = pm.join("agents").join(LEGACY_PROFILE).join("AGENT.md");
        std::fs::create_dir_all(profile.parent().unwrap()).unwrap();
        std::fs::write(&profile, LEGACY_TEXT).unwrap();
        std::fs::write(
            pm.join("pm.yaml"),
            "pi:\n  models:\n    allow: [fixture/portable-only]\n    default:\n      worker: fixture/portable-only\n",
        )
        .unwrap();
        let state = root.path().join("s");
        std::fs::create_dir_all(&state).unwrap();
        let db = state.join("cadence.sqlite3");
        let store = cadence_agent::store::Store::open(&db).unwrap();
        let imported = store.import_legacy_settings(&pm).unwrap();
        assert!(imported.legacy_imported);
        assert_eq!(
            imported
                .doc
                .pi
                .as_ref()
                .unwrap()
                .models
                .default
                .worker
                .as_deref(),
            Some("fixture/portable-only")
        );
        assert!(imported
            .doc
            .profiles
            .iter()
            .any(|row| { row.id == LEGACY_PROFILE && row.text == LEGACY_TEXT }));
        drop(store);
        let before = authority(&state);
        let before_db = std::fs::read(&db).unwrap();
        let before_sources = source_files(&pm);

        // A separate clean tracker keeps the legacy authority files out
        // of this coordination package entirely.
        let tracker = root.path().join("tracker");
        std::fs::create_dir_all(&tracker).unwrap();
        std::fs::write(tracker.join("ticket.md"), "# CAD-1548 portable refusal\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["add", "ticket.md"],
            vec![
                "commit",
                "-q",
                "-m",
                "CAD-1548 independent portable fixture",
            ],
        ] {
            let mut command = std::process::Command::new("git");
            command
                .arg("-C")
                .arg(&tracker)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "init.defaultBranch=main",
                    "-c",
                    "maintenance.auto=false",
                ])
                .args(&args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_AUTHOR_NAME", "cad1548-acceptance")
                .env("GIT_AUTHOR_EMAIL", "acceptance@example.invalid")
                .env("GIT_COMMITTER_NAME", "cad1548-acceptance")
                .env("GIT_COMMITTER_EMAIL", "acceptance@example.invalid");
            let output = cadence_agent::reaper::output(&mut command).unwrap();
            assert!(
                output.status.success(),
                "fixture git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let opts = PortableExportOptions {
            scope: Scope::Coordination,
            tracker,
        };
        let package = root.path().join("package");
        export_portable(&state, &package, &opts)
            .expect("populated canonical settings must be omitted, not reject export");
        let manifest: PortableManifest =
            serde_json::from_slice(&std::fs::read(package.join("manifest.json")).unwrap()).unwrap();
        assert!(manifest
            .omitted_tables
            .iter()
            .any(|table| table == "settings_authority"));
        assert!(!manifest
            .tables
            .iter()
            .any(|table| table == "settings_authority"));
        assert!(manifest
            .omitted_rows
            .iter()
            .any(|(table, rows)| { table == "settings_authority" && *rows == 1 }));
        let restored = root.path().join("restored");
        restore_inactive(&package, &restored, &PortableRestoreOptions::default())
            .expect("projected settings authority must restore inactive");
        for copy in [&package, &restored] {
            // Do not open Store: its authority readers rightly reject
            // these deliberately omitted singleton rows.
            let conn = rusqlite::Connection::open_with_flags(
                copy.join("cadence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            for table in ["settings_authority", "model_defaults"] {
                let rows: i64 = conn
                    .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                assert_eq!(rows, 0, "portable copy retained {table} authority");
            }
        }
        assert_eq!(authority(&state), before, "export changed source authority");
        assert_eq!(
            std::fs::read(&db).unwrap(),
            before_db,
            "export changed source database bytes"
        );
        assert_eq!(
            source_files(&pm),
            before_sources,
            "export changed legacy sources"
        );

        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "ALTER TABLE settings_authority ADD COLUMN forged_grant TEXT",
                [],
            )
            .unwrap();
        }
        let before_refusal = std::fs::read(&db).unwrap();
        let refused = root.path().join("refused-package");
        let err = export_portable(&state, &refused, &opts)
            .expect_err("an unknown settings authority column must refuse");
        assert_eq!(
            err.to_string(),
            "portable export refuses: table settings_authority columns do not match the known catalog"
        );
        assert!(!refused.exists(), "refused export published a package");
        assert!(
            std::fs::read_dir(root.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".refused-package.partial-")
            }),
            "refused export left private staging"
        );
        assert_eq!(
            std::fs::read(&db).unwrap(),
            before_refusal,
            "refused export changed source bytes"
        );
        assert_eq!(
            authority(&state),
            before,
            "refused export changed source authority"
        );
        assert_eq!(
            source_files(&pm),
            before_sources,
            "refused export changed legacy sources"
        );
    }

    #[test]
    fn unauthorized_forged_and_invalid_agent_settings_refuse_without_partial_persistence() {
        let mut fx = Fx::start();
        assert_eq!(
            fx.live_agents(),
            0,
            "fixture launched an agent during startup"
        );
        // Import finished before health. GET must not do it again.
        let imported = authority(&fx.state());
        assert_eq!(
            imported.legacy_imported, 1,
            "daemon startup left legacy_imported unset"
        );
        assert!(
            imported.pi.is_none(),
            "startup import stored a Pi policy from an empty pm.yaml: {:?}",
            imported.pi
        );
        assert_eq!(
            authority_profile_text(&imported, LEGACY_PROFILE).as_deref(),
            Some(LEGACY_TEXT),
            "startup import did not copy agents/{LEGACY_PROFILE}/AGENT.md"
        );
        let imported_ids = authority_profile_ids(&imported);
        for (name, bytes) in LEGACY_NON_PROFILES {
            assert_eq!(
                std::fs::read(fx.pm().join("agents").join(name))
                    .unwrap()
                    .as_slice(),
                *bytes,
                "startup changed non-profile sibling {name}"
            );
            assert!(
                !imported_ids.iter().any(|id| id.as_str() == *name),
                "non-profile sibling {name} became a canonical profile"
            );
        }
        let sources = source_files(&fx.pm());

        let first = fx.op(GET, json!({}));
        let prior_revision = first["revision"].clone();
        assert!(
            revision_token(&prior_revision),
            "revision is not {{model_defaults_revision}}:{{sha256_12}}: {prior_revision}"
        );
        let after_get = authority(&fx.state());
        assert_eq!(after_get, imported, "GET changed the settings authority");
        assert_eq!(
            after_get.defaults_revision.to_string(),
            prior_revision
                .as_str()
                .and_then(|token| token.split_once(':'))
                .map(|(rev, _)| rev.to_string())
                .expect("revision token"),
            "visible revision is not the startup model_defaults revision"
        );
        assert_eq!(
            source_files(&fx.pm()),
            sources,
            "GET touched pm.yaml or agents/"
        );
        assert!(
            has_profile(&first["config"], LEGACY_PROFILE),
            "imported profile missing from the snapshot: {first}"
        );

        let prior = fx.snapshot();
        assert_eq!(
            prior["revision"], prior_revision,
            "later read changed the revision"
        );
        assert_eq!(
            authority(&fx.state()),
            imported,
            "later read changed the settings authority"
        );
        assert_eq!(
            source_files(&fx.pm()),
            sources,
            "later read touched pm.yaml or agents/"
        );
        fx.plant_unmanaged();
        assert!(
            !has_profile(&prior["config"], PROFILE),
            "fresh fixture already had the acceptance profile"
        );

        let session = fx.operator_session();
        let (status, text, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE,
            &save_body(&prior_revision, supported_config(&prior["config"])),
            Some(&session),
        );
        assert_eq!(status, 200, "supported operator save refused: {text}");
        let saved = parse_json(&text);
        assert_no_secret_fields(&saved);
        assert_ne!(
            saved["revision"], prior_revision,
            "supported save did not advance the revision"
        );
        assert!(
            has_profile(&saved["config"], PROFILE),
            "supported save response dropped {PROFILE}: {saved}"
        );
        let reloaded = fx.snapshot();
        assert!(
            revision_token(&reloaded["revision"]),
            "{}",
            reloaded["revision"]
        );
        assert_eq!(reloaded["revision"], saved["revision"]);
        assert_eq!(reloaded["config"], saved["config"]);
        assert_eq!(fx.live_agents(), 0, "supported save launched an agent");
        let held = fx.hold(&reloaded);
        let prefix = held
            .revision
            .as_str()
            .and_then(|token| token.split_once(':'))
            .map(|(rev, _)| rev.to_string())
            .expect("revision token");
        assert_eq!(
            held.authority.defaults_revision.to_string(),
            prefix,
            "visible revision is not the model_defaults revision"
        );
        assert_eq!(
            held.authority.legacy_imported, 1,
            "save cleared legacy_imported"
        );
        assert_eq!(
            held.authority.defaults_revision,
            imported.defaults_revision + 1,
            "committed save did not increment model_defaults.revision by 1"
        );
        assert!(
            authority_profile_text(&held.authority, LEGACY_PROFILE)
                .is_some_and(|text| text.contains("cadence-1548 legacy profile body")),
            "save dropped the imported legacy profile"
        );
        assert_eq!(
            source_files(&fx.pm()),
            sources,
            "save wrote pm.yaml or agents/"
        );
        let stored_ids = authority_profile_ids(&held.authority);
        assert!(
            stored_ids.iter().any(|id| id == PROFILE),
            "saved profile is not in settings_authority: {stored_ids:?}"
        );
        for refused in [FORGED_PROFILE, STALE_PROFILE, "master", ESCAPE_PROFILE] {
            assert!(
                !stored_ids.iter().any(|id| id == refused),
                "settings_authority stored refused profile {refused}: {stored_ids:?}"
            );
        }
        // Profile definitions contain instruction bodies. A valid board
        // session does not turn an agent or unproven peer into the operator.
        let private_jobs: Vec<&str> = held.config["profiles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|profile| profile["job"].as_str())
            .filter(|job| !job.is_empty())
            .collect();
        assert!(private_jobs
            .iter()
            .any(|job| job.contains("cadence-1548 legacy profile body")));
        let operator_get = fx.op(GET, json!({}));
        assert_eq!(operator_get["revision"], held.revision);
        assert_eq!(operator_get["config"], held.config);
        for who in [
            Asserted::Agent("cad1548-profile-reader".into()),
            Asserted::Unproven,
        ] {
            let err = fx
                .rpc(who.clone(), GET, json!({}))
                .expect_err("a non-operator RPC read disclosed profile definitions");
            assert!(
                is_operator_gate(&err.to_string()),
                "settings GET refusal: {err}"
            );
            for job in &private_jobs {
                assert!(
                    !err.to_string().contains(job),
                    "RPC refusal disclosed a profile job"
                );
            }
            let private_session = fx.operator_session();
            let (status, reply, _) =
                fx.http_get(&Asserted::Operator, ROUTE, Some(&private_session));
            assert_eq!(
                status, 200,
                "fresh credential positive before privacy refusal: {reply}"
            );
            assert_eq!(parse_json(&reply)["config"], held.config);
            let (status, reply, _) = fx.http_get(&who, ROUTE, Some(&private_session));
            assert!(
                matches!(status, 401 | 403),
                "non-operator HTTP GET: {status} {reply}"
            );
            for job in &private_jobs {
                assert!(!reply.contains(job), "HTTP refusal disclosed a profile job");
            }
            assert_held(&fx, &held);
        }
        let positive_session = fx.operator_session();
        let (status, reply, _) = fx.http_get(&Asserted::Operator, ROUTE, Some(&positive_session));
        assert_eq!(status, 200, "operator settings GET: {reply}");
        let operator_get = parse_json(&reply);
        assert_eq!(operator_get["config"], held.config);
        assert_eq!(operator_get["read_only"], json!(false));
        assert_held(&fx, &held);

        // Exercise a real read-only board, not a fabricated response flag.
        // Keep the existing daemon and restore the normal listener afterward.
        let normal_port = fx.port;
        let mut read_only_port = None;
        for port in 3110..3200 {
            let (tx, rx) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                read_only: true,
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
                    read_only_port = Some(port);
                    fx.threads.push(thread);
                    break;
                }
                Err(_) => thread.join().unwrap(),
            }
        }
        fx.port = read_only_port.expect("no read-only board port in 3110-3199");
        let read_only_session = fx.operator_session();
        let (status, reply, _) = fx.http_get(&Asserted::Operator, ROUTE, Some(&read_only_session));
        assert_eq!(status, 200, "read-only operator settings GET: {reply}");
        let read_only_get = parse_json(&reply);
        assert_eq!(read_only_get["revision"], held.revision);
        assert_eq!(read_only_get["config"], held.config);
        assert_eq!(read_only_get["read_only"], json!(true));
        for who in [
            Asserted::Agent("cad1548-profile-reader".into()),
            Asserted::Unproven,
        ] {
            let read_only_session = fx.operator_session();
            let (status, reply, _) =
                fx.http_get(&Asserted::Operator, ROUTE, Some(&read_only_session));
            assert_eq!(
                status, 200,
                "fresh read-only credential positive before privacy refusal: {reply}"
            );
            let positive = parse_json(&reply);
            assert_eq!(positive["config"], held.config);
            assert_eq!(positive["read_only"], json!(true));
            let (status, reply, _) = fx.http_get(&who, ROUTE, Some(&read_only_session));
            assert!(
                matches!(status, 401 | 403),
                "read-only non-operator GET: {status} {reply}"
            );
            for job in &private_jobs {
                assert!(
                    !reply.contains(job),
                    "read-only refusal disclosed a profile job"
                );
            }
        }
        let (status, reply, _) = fx.http_get(&Asserted::Operator, ROUTE, None);
        assert!(
            matches!(status, 401 | 403),
            "unsigned read-only GET: {status} {reply}"
        );
        for job in &private_jobs {
            assert!(
                !reply.contains(job),
                "unsigned refusal disclosed a profile job"
            );
        }
        assert_held(&fx, &held);
        fx.port = normal_port;
        assert_held(&fx, &held);

        let sync_one = json!({"expected_revision": held.revision, "profile": PROFILE});
        fx.op(SYNC, sync_one);
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE_SYNC,
            &json!({"expected_revision": held.revision, "profile": null}),
            Some(&fx.operator_session()),
        );
        assert_eq!(status, 200, "sync of saved profiles: {reply}");
        assert_held(&fx, &held);

        let callers = [Asserted::Agent("cad1548-forged".into()), Asserted::Unproven];
        let clean = [
            (
                SAVE,
                ROUTE,
                save_body(&held.revision, forged_config(&held.config)),
            ),
            (
                SYNC,
                ROUTE_SYNC,
                json!({"expected_revision": held.revision, "profile": null}),
            ),
            (
                PREVIEW,
                ROUTE_PREVIEW,
                json!({"expected_revision": held.revision, "profile": PROFILE, "harness": "pi"}),
            ),
            (CATALOG, ROUTE_CATALOG, json!({"provider": "pi"})),
        ];
        for who in &callers {
            for (method, route, body) in &clean {
                let err = fx
                    .rpc(who.clone(), method, body.clone())
                    .expect_err(&format!("{who:?} {method} mutated settings"));
                assert!(
                    is_operator_gate(&err.to_string()),
                    "{who:?} {method} was not an operator refusal: {err}"
                );
                assert_held(&fx, &held);
                let forged_body = forge(body.clone());
                for carried in [None, Some(fx.operator_session())] {
                    let (status, reply, _) =
                        fx.http_post(who, route, &forged_body, carried.as_ref());
                    assert_eq!(
                        status,
                        403,
                        "{who:?} {route} session={} was not forbidden: {reply}",
                        carried.is_some()
                    );
                    let lower = reply.to_lowercase();
                    assert!(
                        lower.contains("operator") || lower.contains("session"),
                        "{who:?} {route}: {reply}"
                    );
                    assert_held(&fx, &held);
                }
            }
        }

        let forged_save = forge(save_body(&held.revision, forged_config(&held.config)));
        // The board rebuilds RPC params from the known fields, so a forged
        // `operator`/`by` on HTTP is not authority. The daemon sees the
        // raw params and must refuse them for every caller, including the
        // operator, without applying the config.
        for who in callers.iter().chain(std::iter::once(&Asserted::Operator)) {
            let err = fx
                .rpc(who.clone(), SAVE, forged_save.clone())
                .expect_err(&format!("{who:?} forged attribution mutated settings"));
            let text = err.to_string();
            assert!(
                is_operator_gate(&text) || is_forged_field(&text),
                "{who:?} forged save was not an attribution refusal: {text}"
            );
            assert_held(&fx, &held);
        }

        let policy = save_body(&held.revision, policy_config(&held.config));
        let err = fx
            .rpc(Asserted::Operator, SAVE, policy.clone())
            .expect_err("operator saved a launch-policy-invalid config");
        assert!(
            is_launch_policy(&err.to_string()),
            "policy save was not a launch-policy refusal: {err}"
        );
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE,
            &policy,
            Some(&fx.operator_session()),
        );
        assert!(
            matches!(status, 400 | 409 | 422),
            "policy HTTP save persisted or failed closed wrongly: {status} {reply}"
        );
        assert!(
            is_launch_policy(&reply),
            "policy HTTP save was not a launch-policy refusal: {reply}"
        );
        assert_held(&fx, &held);

        let absolute = fx.root.path().join("cad1548-escaped-abs");
        let escaped = save_body(&held.revision, escape_config(&held.config, &absolute));
        let err = fx
            .rpc(Asserted::Operator, SAVE, escaped.clone())
            .expect_err("operator saved a path-escape config");
        assert!(
            is_path_refusal(&err.to_string()),
            "path-escape save was not a path/adapter refusal: {err}"
        );
        assert_held(&fx, &held);
        assert_no_escape(&fx, &absolute);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE,
            &escaped,
            Some(&fx.operator_session()),
        );
        assert!(
            matches!(status, 400 | 409 | 422),
            "path-escape HTTP save: {status} {reply}"
        );
        assert!(
            is_path_refusal(&reply),
            "path-escape HTTP save was not a path/adapter refusal: {reply}"
        );
        assert_held(&fx, &held);
        assert_no_escape(&fx, &absolute);

        let master = save_body(&held.revision, master_config(&held.config));
        let err = fx
            .rpc(Asserted::Operator, SAVE, master.clone())
            .expect_err("operator saved a profile id of master");
        let master_text = err.to_string().to_lowercase();
        assert!(
            master_text.contains("master") && !master_text.contains("unknown method"),
            "master profile id was not refused: {err}"
        );
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE,
            &master,
            Some(&fx.operator_session()),
        );
        assert!(
            matches!(status, 400 | 409 | 422),
            "master profile HTTP save: {status} {reply}"
        );
        assert!(
            reply.to_lowercase().contains("master"),
            "master profile HTTP save: {reply}"
        );
        assert_held(&fx, &held);
        assert_no_escape(&fx, &absolute);

        for profile_arg in [ESCAPE_PROFILE, "..", absolute.to_str().unwrap()] {
            let body = json!({"expected_revision": held.revision, "profile": profile_arg});
            let err = fx
                .rpc(Asserted::Operator, SYNC, body.clone())
                .expect_err(&format!("operator synced escaped profile {profile_arg}"));
            let text = err.to_string();
            assert!(
                !text.to_lowercase().contains("unknown method")
                    && (profile_arg == absolute.to_str().unwrap() || is_path_refusal(&text)),
                "sync {profile_arg} was not a path refusal: {text}"
            );
            assert_held(&fx, &held);
            let (status, reply, _) = fx.http_post(
                &Asserted::Operator,
                ROUTE_SYNC,
                &body,
                Some(&fx.operator_session()),
            );
            assert!(
                matches!(status, 400 | 404 | 409 | 422),
                "sync HTTP {profile_arg}: {status} {reply}"
            );
            assert_held(&fx, &held);
            assert_no_escape(&fx, &absolute);
        }

        let preview = json!({
            "expected_revision": held.revision,
            "profile": PROFILE,
            "harness": ESCAPE_CLI
        });
        match fx.rpc(Asserted::Operator, PREVIEW, preview.clone()) {
            Ok(value) => {
                assert_ne!(
                    value["allowed"],
                    json!(true),
                    "preview authorized an escaped harness: {value}"
                );
                assert!(
                    value["command"].is_null(),
                    "preview returned a command for an escaped harness: {value}"
                );
            }
            Err(err) => assert!(
                is_path_refusal(&err.to_string()) || is_launch_policy(&err.to_string()),
                "preview of an escaped harness: {err}"
            ),
        }
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE_PREVIEW,
            &preview,
            Some(&fx.operator_session()),
        );
        if status == 200 {
            let value = parse_json(&reply);
            assert_ne!(
                value["allowed"],
                json!(true),
                "HTTP preview authorized escape: {reply}"
            );
            assert!(value["command"].is_null(), "HTTP preview command: {reply}");
        } else {
            assert!(
                matches!(status, 400 | 404 | 409 | 422),
                "HTTP preview of escaped harness: {status} {reply}"
            );
        }
        assert_held(&fx, &held);
        assert_eq!(fx.live_agents(), 0, "preview launched an agent");

        let stale_save = save_body(&prior_revision, stale_config(&held.config));
        let err = fx
            .rpc(Asserted::Operator, SAVE, stale_save.clone())
            .expect_err("stale revision save was applied");
        assert!(
            is_revision_conflict(&err.to_string()),
            "stale save was not a revision conflict: {err}"
        );
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE,
            &stale_save,
            Some(&fx.operator_session()),
        );
        assert_eq!(status, 409, "stale HTTP save: {reply}");
        let body = parse_json(&reply);
        assert!(
            is_revision_conflict(&reply) || body["code"] == json!("revision_conflict"),
            "stale HTTP save was not a revision conflict: {reply}"
        );
        assert_held(&fx, &held);

        let stale_sync = json!({"expected_revision": prior_revision, "profile": PROFILE});
        let err = fx
            .rpc(Asserted::Operator, SYNC, stale_sync.clone())
            .expect_err("stale revision sync was applied");
        assert!(
            is_revision_conflict(&err.to_string()),
            "stale sync was not a revision conflict: {err}"
        );
        assert_held(&fx, &held);
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE_SYNC,
            &stale_sync,
            Some(&fx.operator_session()),
        );
        assert!(
            matches!(status, 400 | 409 | 422),
            "stale HTTP sync: {status} {reply}"
        );
        assert!(
            is_revision_conflict(&reply)
                || parse_json(&reply)["code"] == json!("revision_conflict"),
            "stale HTTP sync was not a revision conflict: {reply}"
        );
        assert_held(&fx, &held);

        let final_snap = fx.snapshot();
        assert_eq!(final_snap["revision"], held.revision);
        assert!(has_profile(&final_snap["config"], PROFILE));
        assert!(!has_profile(&final_snap["config"], FORGED_PROFILE));
        assert!(!has_profile(&final_snap["config"], STALE_PROFILE));
        assert!(!has_profile(&final_snap["config"], ESCAPE_PROFILE));
        refuse_export_symlinks(&fx, &held.revision);
        refuse_symlinked_import();
        prove_profile_launch_provenance(&fx, &held.revision);
        pi_kind_role_default_reaches_pi_only();
        portable_settings_authority_is_omitted_and_unknown_columns_refuse();
        assert_eq!(fx.snapshot()["revision"], held.revision);
        assert_eq!(fx.live_agents(), 0);

        // A saved allowlisted model is still refused by the host's actual
        // confinement default. Preview must explain that same refusal,
        // without starting an agent or changing the saved authority.
        let model = "devin/swe-2-high";
        let profile_id = "confined-preview";
        let mut config = held.config.clone();
        config["providers"].as_array_mut().unwrap().push(json!({
            "id": "devin-preview",
            "label": "Devin preview acceptance",
            "kind": "Pi extension",
            "pin": "",
            "gate": true,
            "paid": false,
            "note": ""
        }));
        config["models"].as_array_mut().unwrap().push(json!({
            "id": model,
            "provider": "devin-preview",
            "ctx": "",
            "out": "",
            "thinking": null,
            "images": null
        }));
        config["scope"][model] = json!("worker");
        profiles(&mut config).push(json!({
            "id": profile_id,
            "role": "worker",
            "model": model,
            "job": "Preview must not launch this confined Devin worker."
        }));
        let saved = fx.op(SAVE, save_body(&held.revision, config));
        let stored = authority(&fx.state());
        let policy: cadence_agent::pi_policy::PiPolicy =
            serde_json::from_str(stored.pi.as_deref().expect("saved Pi policy")).unwrap();
        cadence_agent::pi_policy::require_allowed(Some(&policy), "worker", model)
            .expect("the preview model must be allowed by canonical worker policy");

        let pm_yaml = fx.pm().join("pm.yaml");
        let mut yaml: serde_yaml::Value =
            serde_yaml::from_slice(&std::fs::read(&pm_yaml).unwrap()).unwrap();
        yaml["host"] = serde_yaml::from_str("confine_pi_workers: true\n").unwrap();
        std::fs::write(&pm_yaml, serde_yaml::to_string(&yaml).unwrap()).unwrap();
        // The earlier symlink exercise intentionally replaced the export
        // fixture. Capture its current tree, including missing entries and
        // symlink targets, rather than requiring the original files again.
        let export_files = || {
            let mut files = BTreeMap::new();
            for name in [".pi", ".claude"] {
                walk_files(&fx.pm().join(name), name, &mut files);
            }
            files
        };
        let guarded_exports = export_files();
        let guarded_sources = source_files(&fx.pm());
        let guarded_pm_yaml = std::fs::read(&pm_yaml).unwrap();
        let guarded_authority = authority(&fx.state());
        let assert_guarded = || {
            let snapshot = fx.snapshot();
            assert_eq!(snapshot["revision"], saved["revision"]);
            assert_eq!(snapshot["config"], saved["config"]);
            assert_eq!(authority(&fx.state()), guarded_authority);
            assert_eq!(std::fs::read(&pm_yaml).unwrap(), guarded_pm_yaml);
            assert_eq!(source_files(&fx.pm()), guarded_sources);
            assert_eq!(export_files(), guarded_exports);
            assert!(agent_aliases(&fx.state()).is_empty());
            assert_eq!(fx.live_agents(), 0);
        };
        assert!(agent_aliases(&fx.state()).is_empty());

        let err = fx
            .rpc(
                Asserted::Operator,
                "agent_register",
                json!({
                    "alias": "c1548-confined-preview",
                    "provider": "pi",
                    "endpoint_kind": "managed",
                    "role": "worker",
                    "cwd": fx.pm().to_string_lossy(),
                    "profile": profile_id,
                }),
            )
            .expect_err("the real host confinement guard accepted Devin");
        let refusal = err.to_string().to_lowercase();
        assert!(
            refusal.contains("confined") && refusal.contains("devin"),
            "registration failed for a different reason: {err}"
        );
        assert!(agent_aliases(&fx.state()).is_empty());
        assert_guarded();

        let body = json!({
            "expected_revision": saved["revision"],
            "profile": profile_id,
            "harness": "pi",
        });
        let rpc_preview = fx.op(PREVIEW, body.clone());
        let (status, reply, _) = fx.http_post(
            &Asserted::Operator,
            ROUTE_PREVIEW,
            &body,
            Some(&fx.operator_session()),
        );
        assert_eq!(status, 200, "confined preview HTTP response: {reply}");
        for preview in [rpc_preview, parse_json(&reply)] {
            assert_eq!(preview["model"], json!(model));
            assert_eq!(preview["allowed"], json!(false), "{preview}");
            assert!(preview["command"].is_null(), "{preview}");
            assert!(
                preview["reasons"].as_array().is_some_and(|reasons| {
                    reasons.iter().filter_map(Value::as_str).any(|reason| {
                        let reason = reason.to_lowercase();
                        reason.contains("confined") && reason.contains("devin")
                    })
                }),
                "preview omitted the real host confinement refusal: {preview}"
            );
        }
        assert!(
            agent_aliases(&fx.state()).is_empty(),
            "preview created an agent"
        );
        assert_guarded();

        // The supported legacy writer edits model defaults, not the Agents
        // overlay. Exercise its real RPC and HTTP peer after every assertion
        // bound to `held`: each positive control advances the shared revision.
        let session = fx.operator_session();
        let settings_audit = || -> Vec<(i64, String, String, u64)> {
            let conn = open_state(&fx.state());
            let mut stmt = conn
                .prepare(
                    "SELECT seq, kind, payload, at FROM events
                     WHERE kind IN ('model_defaults_updated', 'settings_updated',
                                    'settings_legacy_imported') ORDER BY seq",
                )
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, f64>(3)?.to_bits(),
                    ))
                })
                .unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        for via_http in [false, true] {
            let before = authority(&fx.state());
            let original: Value = serde_json::from_str(&before.defaults_document).unwrap();
            assert!(original["agent_settings"].is_object());
            let mut core = original.clone();
            core.as_object_mut().unwrap().remove("agent_settings");
            let positive = json!({
                "expected_revision": before.defaults_revision,
                "config": core,
            });
            // Establish that the core envelope is valid independently of
            // overlay rejection; do not pre-validate the malformed overlay.
            cadence_agent::model_defaults::parse_settings_document(&positive.to_string())
                .expect("legacy positive control must be a valid ModelDefaults document");
            let audit_before = settings_audit();
            let sources_before = source_files(&fx.pm());
            if via_http {
                let (status, reply, _) = fx.http_post(
                    &Asserted::Operator,
                    "/api/settings/model-defaults",
                    &positive,
                    Some(&session),
                );
                assert_eq!(status, 200, "valid legacy HTTP update: {reply}");
            } else {
                fx.op(
                    "model_defaults_set",
                    json!({"document": positive.to_string()}),
                );
            }
            let accepted = authority(&fx.state());
            assert_eq!(accepted.defaults_revision, before.defaults_revision + 1);
            let accepted_document: Value =
                serde_json::from_str(&accepted.defaults_document).unwrap();
            assert_eq!(
                accepted_document, original,
                "legacy update changed the overlay"
            );
            assert_eq!(accepted.pi, before.pi);
            assert_eq!(accepted.profiles, before.profiles);
            assert_eq!(accepted.legacy_imported, before.legacy_imported);
            assert_eq!(source_files(&fx.pm()), sources_before);
            let audit_accepted = settings_audit();
            assert_eq!(audit_accepted.len(), audit_before.len() + 1);
            assert_eq!(
                &audit_accepted[..audit_before.len()],
                audit_before.as_slice(),
                "legacy positive update rewrote earlier audit"
            );
            assert_eq!(audit_accepted.last().unwrap().1, "model_defaults_updated");

            let visible = fx.snapshot();
            let audit_held = settings_audit();
            let mut invalid = positive.clone();
            invalid["expected_revision"] = json!(accepted.defaults_revision);
            invalid["config"]["agent_settings"] = json!({"models": [null]});
            let refusal = if via_http {
                let (status, reply, _) = fx.http_post(
                    &Asserted::Operator,
                    "/api/settings/model-defaults",
                    &invalid,
                    Some(&session),
                );
                assert_eq!(status, 400, "malformed legacy overlay HTTP update: {reply}");
                reply
            } else {
                fx.rpc(
                    Asserted::Operator,
                    "model_defaults_set",
                    json!({"document": invalid.to_string()}),
                )
                .expect_err("legacy RPC accepted a malformed Agents overlay")
                .to_string()
            };
            let lower = refusal.to_ascii_lowercase();
            assert!(
                lower.contains("agent_settings") || lower.contains("overlay"),
                "legacy refusal was not about the overlay: {refusal}"
            );
            assert_eq!(
                authority(&fx.state()),
                accepted,
                "refused legacy write persisted"
            );
            assert_eq!(
                settings_audit(),
                audit_held,
                "refused legacy write changed audit"
            );
            let reloaded = fx.snapshot();
            assert_eq!(reloaded["revision"], visible["revision"]);
            assert_eq!(reloaded["config"], visible["config"]);
            assert_eq!(source_files(&fx.pm()), sources_before);
            assert_eq!(
                fx.live_agents(),
                0,
                "legacy settings write launched an agent"
            );
        }
    }
}
