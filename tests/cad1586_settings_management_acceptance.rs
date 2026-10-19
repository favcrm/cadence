//! CAD-1586 / AOS-290 independent guard acceptance, authored against the
//! ticket and the shared contract (`local://cad1586-settings-contract.md`
//! v1.2 plus the SettingsBackend answers it exchanged with this author),
//! not against an implementer's summary. Implementers must not edit or
//! weaken this file.
//!
//! One real daemon and one real board (`--features test-seam`). Local
//! caller identity is the explicit seam only: this file never clears
//! `CADENCE_ALIAS`, never sets `CADENCE_TEST_AS`, and never uses a
//! daemon-child grant token. Remote (cli) authority is a REAL Ed25519
//! envelope verified against a REAL JWKS server and the daemon's own
//! `board-identity.json` — signature, issuer, audience, org, time and
//! jti replay are exercised, never mocked; only the board's own
//! socket identity to the daemon rides the supported seam fixture.
//!
//! Proves, on `agent_settings_command` and its public HTTP peer
//! `POST /api/cli/settings_agents` -> daemon `cli_settings_command`:
//!
//! - while NO master owns a session, an approved managing agent executes
//!   its exact bound operation (a master-repair `model.scope` and a
//!   file-body `profile.put`) and observes the post-change snapshot —
//!   the petition (`grant.request`), operator decision (`grant.allow`)
//!   and authorized use all work with no live master, and the grant
//!   store survives a daemon restart (`settings-grants.json`);
//! - `master.resume` (the bounded recovery op, `try_resume` verbatim)
//!   and `master.status` (the launch diagnosis read) ride the same
//!   grant machinery: refused ungranted for an ordinary worker and a
//!   remote agent, `expected_revision`-bound to null, authorized only
//!   by an exact bound grant — a refused authorization attempt never
//!   consumes the grant;
//! - the `master` alias itself is an ordinary agent subject: ungranted
//!   ops refuse, it may petition `grant.request`/`grant.list`, and an
//!   operator-approved grant authorizes only that exact op — grant
//!   issuance is never delegable, so no caller may self-issue;
//! - an ordinary worker is refused EVERY op except `grant.request` /
//!   `grant.list`; an unproven connection, an unregistered asserted
//!   alias and a member-bound assistant alias (no `agents` row —
//!   `Unproven`) are refused everything including the petition;
//! - the send preflight (CAD-1586): a registered, offline pi master
//!   whose stored `params.model` is off-scope refuses every send entry
//!   (`agent_send`, worker or operator; the remote `message_send`
//!   funnel) with `master_unlaunchable` BEFORE the durable enqueue —
//!   no message row, no queue entry — and the granted `model.scope`
//!   repair flips it to stopped-but-launchable so the same send queues
//!   (the provider fixture keeps actual launch refused);
//! - identity-shaped fields (`by`/`as`/`actor`/`caller`/`operator`/
//!   `reviewer`/`pane`/`lane`/`pid`/`owner`/`agent`) are refused for
//!   the operator too, on the outer params, on the request body and
//!   inside the intended payload — authority is the connection/envelope,
//!   never a request field;
//! - a grant binds its exact operation, requester (alias + registration
//!   generation + uid), canonical payload digest, expected_revision and
//!   org at decision time: a different operation, resource id, field
//!   value or caller, an altered post-approval payload, a forged id,
//!   an expired (persisted `expires_at` moved to the past), a revoked
//!   or a replayed (single-use) grant all refuse BEFORE mutation —
//!   and a refused authorization attempt does not burn the grant;
//! - a petition for a decision op (`grant.allow`/`reject`/`revoke`)
//!   refuses — grant issuance is never delegable;
//! - CAS: a stale `expected_revision` (operator or granted agent) is a
//!   `conflict`-kind error and commits nothing anywhere;
//! - HTTP peer: no `wikienv_` bearer (including a fresh operator board
//!   session) is refused; a settings.read-only envelope cannot mutate,
//!   petition or `master.resume`; `cli.read`/`cli.write` never satisfy
//!   a settings op; `users/<h>` member and `service/<h>` envelopes are
//!   refused all ops; `agents/<h>` gets `grant.request`/`grant.list`
//!   plus exactly its grant-bound op; a local-agent grant never
//!   satisfies a remote subject; replayed jti, wrong kid, wrong org and
//!   expired envelopes refuse at the cryptographic layer with no
//!   settings effect;
//! - every refusal leaves the snapshot revision, the config, the
//!   `model_defaults`/`settings_authority` rows, `pm.yaml`, `agents/`
//!   and the grants file untouched, and launches no agent.
//!
//! Admitted run (manager; not executed by this author):
//! `cadence build-slot run test -- env CARGO_BUILD_JOBS=4 rtk proxy cargo test --locked --features test-seam --test cad1586_settings_management_acceptance -- --test-threads 1`

#[cfg(all(feature = "test-seam", target_os = "linux"))]
mod acceptance {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
    use cadence_agent::{board_identity, client, daemon, issue, operator_auth, store, ui};
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use serde_json::{json, Value};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::{Duration, Instant, SystemTime};

    /// The one managed-settings verb and its remote-envelope peer.
    const CMD: &str = "agent_settings_command";
    const CLI_CMD: &str = "cli_settings_command";
    /// The preserved operator UI verbs — the cross-check and the
    /// unchanged-surface witnesses.
    const GET: &str = "agent_settings_get";
    const SET: &str = "agent_settings_set";

    const ROUTE: &str = "/api/settings/agents";
    /// The public-host cli routes (the gateway's peers forward the same
    /// operation envelope this board route serves).
    const CLI_ROUTE: &str = "/api/cli/settings_agents";
    const CLI_MASTER: &str = "/api/cli/master_status";

    /// Public surface of the fixture board — non-`*.localhost`, so the
    /// envelope audience is `https://` per `public_scheme`.
    const HOST: &str = "cad1586.cadencecloud.test";
    const ORG: &str = "ws_cad1586_fixture";
    const KID: &str = "cad1586-independent-fixture";

    const WORKER: &str = "c1586-worker";
    const OTHER: &str = "c1586-other";
    const GHOST: &str = "c1586-ghost";
    const ASSIST: &str = "c1586-assist";
    const MASTER: &str = "master";
    /// The repair use case: a Pi-gated model the master may launch.
    const PI_MODEL: &str = "fixture/c1586-pi-model";

    /// Identity-shaped fields the contract refuses anywhere in params —
    /// the union of `IDENTITY_FIELDS` and the contract's own list.
    const FORGED_FIELDS: &[&str] = &[
        "by", "as", "actor", "caller", "operator", "reviewer", "pane", "lane", "pid", "owner",
        "agent",
    ];

    type Session = (String, String);

    /// `model_defaults` plus the v52 `settings_authority` row — the
    /// authoritative no-mutation witness (the cad1548 witness columns).
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct Authority {
        defaults_revision: i64,
        defaults_document: String,
        pi: Option<String>,
        profiles: String,
        legacy_imported: i64,
    }

    /// Everything a refused call must leave untouched.
    #[derive(Clone, Debug, PartialEq)]
    struct Held {
        revision: Value,
        config: Value,
        pm_yaml: Vec<u8>,
        sources: BTreeMap<String, (SystemTime, Vec<u8>)>,
        authority: Authority,
    }

    struct Fx {
        root: tempfile::TempDir,
        /// Stops the board + JWKS threads at fixture drop.
        stop: Arc<AtomicBool>,
        /// Stops only the daemon — `restart_daemon` proves grant-store
        /// durability while the board keeps serving.
        daemon_stop: Arc<AtomicBool>,
        daemon: Option<std::thread::JoinHandle<()>>,
        threads: Vec<std::thread::JoinHandle<()>>,
        port: u16,
        issuer: String,
        key: Ed25519KeyPair,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            self.stop.store(true, SeqCst);
            self.daemon_stop.store(true, SeqCst);
            if let Some(thread) = self.daemon.take() {
                let _ = thread.join();
            }
            for thread in self.threads.drain(..).rev() {
                let _ = thread.join();
            }
        }
    }

    impl Fx {
        fn start() -> Self {
            let root = tempfile::Builder::new().prefix("c1586-").tempdir().unwrap();
            let mut fx = Self {
                root,
                stop: Arc::new(AtomicBool::new(false)),
                daemon_stop: Arc::new(AtomicBool::new(false)),
                daemon: None,
                threads: vec![],
                port: 0,
                issuer: String::new(),
                key: Ed25519KeyPair::from_seed_unchecked(&[158; 32]).unwrap(),
            };
            issue::Pm::init(&fx.pm()).unwrap();
            fx.start_daemon();

            // Real JWKS over loopback — `fetch_keys` gets this exact
            // document; only the transport is a test server, the key,
            // cache and verifier are production.
            let jwks = tiny_http::Server::http(("127.0.0.1", free_port())).unwrap();
            fx.issuer = format!("http://{}", jwks.server_addr());
            let public_key = URL_SAFE_NO_PAD.encode(fx.key.public_key().as_ref());
            let jwks_stop = fx.stop.clone();
            fx.threads.push(std::thread::spawn(move || {
                while !jwks_stop.load(SeqCst) {
                    if let Ok(Some(request)) = jwks.recv_timeout(Duration::from_millis(100)) {
                        assert_eq!(request.url(), "/.well-known/agenticos-board-jwks.json");
                        let response = tiny_http::Response::from_string(
                            json!({"keys": [{
                                "kty": "OKP", "crv": "Ed25519", "kid": KID, "x": public_key
                            }]})
                            .to_string(),
                        )
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", "application/json")
                                .unwrap(),
                        );
                        request.respond(response).unwrap();
                    }
                }
            }));

            // The daemon's own trust root — what `cli_settings_command`
            // re-verifies against, never a request field.
            board_identity::write_config(
                &fx.state(),
                &board_identity::Config {
                    host: HOST.into(),
                    issuer: fx.issuer.clone(),
                    company: ORG.into(),
                },
            )
            .unwrap();

            for port in 3110..3200 {
                let (tx, rx) = std::sync::mpsc::channel();
                let opts = ui::ServeOpts {
                    host: "127.0.0.1".into(),
                    port,
                    allow_hosts: vec![HOST.into(), format!("cadence-{port}.localhost:{port}")],
                    public: Some(ui::PublicBoard {
                        host: HOST.into(),
                        issuer: fx.issuer.clone(),
                        company: ORG.into(),
                        authorize_url: format!("{}/authorize", fx.issuer),
                    }),
                    stop: Some(fx.stop.clone()),
                    startup: Some(tx),
                    test_seam: true,
                    gh: Some("/bin/false".into()),
                    ..Default::default()
                };
                let (state, pm) = (fx.state(), fx.pm());
                let thread = std::thread::spawn(move || {
                    let _ = ui::serve(&state, &pm, &opts);
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
        fn grants_path(&self) -> PathBuf {
            self.state().join("settings-grants.json")
        }

        fn start_daemon(&mut self) {
            let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
            env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
            let opts = daemon::ServeOptions {
                provider_env: env,
                stop: Some(self.daemon_stop.clone()),
                test_seam: true,
                slots: Some(Default::default()),
                lease: Some(Default::default()),
                auto_stop: Some(daemon::AutoStopSetting::off()),
                agent_gc: Some(Default::default()),
                report_router: Some(0),
                checkup: Some(0),
                ..Default::default()
            };
            let state = self.state();
            self.daemon = Some(std::thread::spawn(move || {
                daemon::serve_with(&state, opts).unwrap();
            }));
            let deadline = Instant::now() + Duration::from_secs(30);
            while client::rpc_timeout(&self.state(), "health", json!({}), Duration::from_secs(2))
                .is_err()
                || Seam::token_at(&self.state()).is_none()
            {
                assert!(Instant::now() < deadline, "daemon never started");
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// Stop ONLY the daemon and bring up a fresh one on the same
        /// state dir — the durability witness for `settings-grants.json`.
        /// The board and JWKS server keep running on their own flag.
        fn restart_daemon(&mut self) {
            self.daemon_stop.store(true, SeqCst);
            if let Some(thread) = self.daemon.take() {
                thread.join().expect("daemon thread panicked");
            }
            self.daemon_stop.store(false, SeqCst);
            self.start_daemon();
        }

        fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
            let state = self.state();
            scoped(who, || client::rpc(&state, method, params))
        }
        fn op(&self, method: &str, params: Value) -> Value {
            self.rpc(Asserted::Operator, method, params)
                .unwrap_or_else(|e| panic!("operator {method}: {e}"))
        }

        /// The closed `agent_settings_command` params.
        fn cmd(
            &self,
            who: Asserted,
            operation: &str,
            expected_revision: Value,
            payload: Value,
        ) -> cadence_agent::Result<Value> {
            self.rpc(
                who,
                CMD,
                json!({
                    "operation": operation,
                    "expected_revision": expected_revision,
                    "payload": payload,
                }),
            )
        }
        fn cmd_err(
            &self,
            who: Asserted,
            operation: &str,
            expected_revision: Value,
            payload: Value,
        ) -> cadence_agent::Error {
            self.cmd(who, operation, expected_revision, payload)
                .unwrap_err()
        }

        fn store(&self) -> store::Store {
            store::Store::open(&self.state().join("cadence.sqlite3")).unwrap()
        }

        fn register(&self, alias: &str, role: &str) {
            self.op(
                "agent_register",
                json!({
                    "alias": alias,
                    "provider": "inbox",
                    "endpoint_kind": "inbox",
                    "role": role,
                    "cwd": self.pm().to_string_lossy(),
                }),
            );
        }

        /// A stored snapshot — via the command surface.
        fn snapshot(&self) -> Value {
            let snap = self
                .cmd(Asserted::Operator, "snapshot", Value::Null, json!({}))
                .unwrap_or_else(|e| panic!("operator snapshot: {e}"));
            assert!(
                revision_token(&snap["revision"]),
                "snapshot revision shape: {}",
                snap["revision"]
            );
            snap
        }

        fn operator_session(&self) -> Session {
            operator_auth::ensure_secret(&self.state()).unwrap();
            let secret = operator_auth::read_secret(&self.state()).unwrap();
            let nonce = self.op(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )["nonce"]
                .clone();
            let (status, reply, cookie) = self.local_post(
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

        fn local_host(&self) -> String {
            format!("cadence-{}.localhost:{}", self.port, self.port)
        }

        /// The local surface: operator-session routes like
        /// `/api/settings/agents`.
        fn local_post(
            &self,
            who: &Asserted,
            route: &str,
            body: &Value,
            session: Option<&Session>,
        ) -> (u16, String, Option<String>) {
            self.exchange(
                who,
                "POST",
                &self.local_host(),
                route,
                Some(body),
                session,
                None,
            )
        }
        fn local_get(
            &self,
            who: &Asserted,
            route: &str,
            session: Option<&Session>,
        ) -> (u16, String, Option<String>) {
            self.exchange(who, "GET", &self.local_host(), route, None, session, None)
        }

        /// The public-host `/api/cli/<verb>` surface: the `wikienv_`
        /// bearer is the only credential — no session is sent, the seam
        /// headers carry ONLY the board's own socket identity.
        fn cli_post(
            &self,
            who: &Asserted,
            route: &str,
            body: &Value,
            bearer: Option<&str>,
        ) -> (u16, String) {
            let (status, text, _) =
                self.exchange(who, "POST", HOST, route, Some(body), None, bearer);
            (status, text)
        }

        #[allow(clippy::too_many_arguments)]
        fn exchange(
            &self,
            who: &Asserted,
            method: &str,
            host: &str,
            route: &str,
            body: Option<&Value>,
            session: Option<&Session>,
            bearer: Option<&str>,
        ) -> (u16, String, Option<String>) {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .http_status_as_error(false)
                .build()
                .into();
            let url = format!("http://127.0.0.1:{}{route}", self.port);
            let token = Seam::token_at(&self.state()).unwrap();
            let mut response = if method == "GET" {
                let mut req = agent
                    .get(&url)
                    .header("Host", host)
                    .header("Origin", format!("http://{host}"))
                    .header("X-Cadence-Board", "1")
                    .header(AS_HEADER, who.as_str())
                    .header(TOKEN_HEADER, &token);
                if let Some((cookie, key)) = session {
                    req = req
                        .header("Cookie", cookie)
                        .header("X-Cadence-Session", key);
                }
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", bearer);
                }
                req.call().unwrap()
            } else {
                let mut req = agent
                    .post(&url)
                    .header("Host", host)
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
                if let Some(bearer) = bearer {
                    req = req.header("Authorization", bearer);
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

        /// A signed cli actor envelope — the real JWS the route and the
        /// daemon both re-verify.
        fn claims(&self, actor: &str, kind: &str, role: &str, scopes: &[&str], jti: &str) -> Value {
            let now = issue::time::now_epoch();
            json!({
                "iss": self.issuer,
                "aud": format!("https://{HOST}"),
                "sub": format!("sub-{actor}").replace('/', "-"),
                "actor": actor,
                "principal_kind": kind,
                "role_at_issue": role,
                "organization_id": ORG,
                "scope": scopes,
                "credential_id": format!("cred-{jti}"),
                "iat": now,
                "exp": now + 120,
                "jti": jti,
            })
        }
        fn sign_with(&self, key: &Ed25519KeyPair, claims: &Value, kid: &str) -> String {
            let h = URL_SAFE_NO_PAD.encode(
                json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": kid}).to_string(),
            );
            let p = URL_SAFE_NO_PAD.encode(claims.to_string());
            let signed = format!("{h}.{p}");
            format!(
                "{signed}.{}",
                URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).as_ref())
            )
        }
        fn envelope(&self, actor: &str, kind: &str, role: &str, scopes: &[&str]) -> String {
            let jti = format!("jti-{}", uuid::Uuid::new_v4().simple());
            self.sign_with(
                &self.key,
                &self.claims(actor, kind, role, scopes, &jti),
                KID,
            )
        }
        fn bearer(env: &str) -> String {
            format!("Bearer wikienv_{env}")
        }

        /// `cli_settings_command` on the daemon socket — the board's
        /// relayed call. `who` is the CONNECTION identity; the envelope
        /// is the remote authority.
        fn cli_cmd(
            &self,
            who: Asserted,
            envelope: &str,
            operation: &str,
            expected_revision: Value,
            payload: Value,
        ) -> cadence_agent::Result<Value> {
            self.rpc(
                who,
                CLI_CMD,
                json!({
                    "cli_envelope": envelope,
                    "operation": operation,
                    "expected_revision": expected_revision,
                    "payload": payload,
                }),
            )
        }

        fn grants(&self) -> Value {
            let text = std::fs::read_to_string(self.grants_path()).unwrap_or_else(|_| "{}".into());
            serde_json::from_str(&text).expect("settings-grants.json is json")
        }
        fn rewrite_grants(&self, f: impl FnOnce(&mut Value)) {
            let mut doc = self.grants();
            f(&mut doc);
            std::fs::write(self.grants_path(), doc.to_string()).unwrap();
        }

        /// A raw fixture write for turn state transitions a fixture
        /// cannot reach without a provider (`submitting`/`running`/
        /// `completed`) — a real store row change, never an in-memory
        /// or scope seam.
        fn db_execute(&self, sql: &str, args: &[&str]) {
            let conn = rusqlite::Connection::open(self.state().join("cadence.sqlite3")).unwrap();
            conn.busy_timeout(Duration::from_secs(2)).unwrap();
            conn.execute(sql, rusqlite::params_from_iter(args.iter().copied()))
                .unwrap_or_else(|e| panic!("fixture db write failed: {e}"));
        }

        fn hold(&self, snap: &Value) -> Held {
            Held {
                revision: snap["revision"].clone(),
                config: snap["config"].clone(),
                pm_yaml: std::fs::read(self.pm().join("pm.yaml")).unwrap_or_default(),
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
        let (pi, profiles, legacy_imported): (Option<String>, String, i64) = conn
            .query_row(
                "SELECT pi, profiles, legacy_imported FROM settings_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("settings_authority row");
        Authority {
            defaults_revision,
            defaults_document,
            pi,
            profiles,
            legacy_imported,
        }
    }

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
            if meta.is_dir() && !meta.file_type().is_symlink() {
                walk_files(&path, &rel, out);
            } else {
                record_file(&path, &rel, out);
            }
        }
    }
    fn record_file(path: &Path, rel: &str, out: &mut BTreeMap<String, (SystemTime, Vec<u8>)>) {
        let meta = std::fs::symlink_metadata(path).unwrap();
        out.insert(
            rel.to_string(),
            (meta.modified().unwrap(), read_lossy(path)),
        );
    }
    fn read_lossy(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_else(|_| {
            std::fs::read_link(path)
                .map(|t| t.to_string_lossy().into_owned().into_bytes())
                .unwrap_or_default()
        })
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

    fn free_port() -> u16 {
        (3300..=3399)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .expect("no isolated port")
    }

    fn parse_json(text: &str) -> Value {
        serde_json::from_str(text).unwrap_or_else(|_| json!({"error": text}))
    }

    /// A refused call left the whole settings surface untouched.
    fn assert_held(fx: &Fx, held: &Held) {
        let snap = fx.snapshot();
        assert_eq!(snap["revision"], held.revision, "revision moved: {snap}");
        assert_eq!(snap["config"], held.config, "visible config moved: {snap}");
        assert_eq!(
            std::fs::read(fx.pm().join("pm.yaml")).unwrap_or_default(),
            held.pm_yaml,
            "pm.yaml changed after a refusal"
        );
        assert_eq!(
            source_files(&fx.pm()),
            held.sources,
            "pm.yaml or agents/ changed after a refusal"
        );
        assert_eq!(
            authority(&fx.state()),
            held.authority,
            "model_defaults or settings_authority changed after a refusal"
        );
    }

    fn now() -> i64 {
        issue::time::now_epoch()
    }

    /// `grant.request` as `who`: returns the stored request id.
    fn petition(
        fx: &Fx,
        who: &Asserted,
        operation: &str,
        expected_revision: &Value,
        payload: Value,
        reason: &str,
    ) -> String {
        let request = fx
            .cmd(
                who.clone(),
                "grant.request",
                Value::Null,
                json!({
                    "operation": operation,
                    "expected_revision": expected_revision,
                    "payload": payload,
                    "reason": reason,
                }),
            )
            .unwrap_or_else(|e| panic!("grant.request {operation}: {e}"));
        request["request_id"]
            .as_str()
            .expect("grant.request returned no request_id")
            .to_string()
    }

    /// Operator `grant.allow` on a pending request: returns the grant id.
    fn allow(fx: &Fx, request_id: &str) -> String {
        fx.cmd(
            Asserted::Operator,
            "grant.allow",
            Value::Null,
            json!({"request_id": request_id}),
        )
        .unwrap_or_else(|e| panic!("grant.allow: {e}"))["grant_id"]
            .as_str()
            .expect("grant_id")
            .to_string()
    }

    /// Mint a grant end to end: petition as `who`, operator allow.
    /// Returns `(request_id, grant_id)`.
    fn granted(
        fx: &Fx,
        who: &Asserted,
        operation: &str,
        expected_revision: &Value,
        payload: Value,
        reason: &str,
    ) -> (String, String) {
        let request_id = petition(fx, who, operation, expected_revision, payload, reason);
        let grant_id = allow(fx, &request_id);
        (request_id, grant_id)
    }

    /// Seed the launch-repair target: a `Pi extension` provider (the only
    /// kind `model.scope` gates) plus one off-scope model.
    fn seed_pi_model(fx: &Fx) {
        let rev = fx.snapshot()["revision"].clone();
        let saved = fx
            .cmd(
                Asserted::Operator,
                "provider.put",
                rev.clone(),
                json!({
                    "id": "pi",
                    "label": "Pi",
                    "kind": "Pi extension",
                    "pin": "",
                    "gate": true,
                    "paid": false,
                    "note": "",
                }),
            )
            .unwrap_or_else(|e| panic!("provider.put pi: {e}"));
        assert!(
            revision_token(&saved["revision"]) && saved["revision"] != rev,
            "provider.put did not return the post-change snapshot: {saved}"
        );
        let saved = fx
            .cmd(
                Asserted::Operator,
                "model.put",
                saved["revision"].clone(),
                json!({
                    "id": PI_MODEL,
                    "provider": "pi",
                    "ctx": "",
                    "out": "",
                    "thinking": null,
                    "images": null,
                }),
            )
            .unwrap_or_else(|e| panic!("model.put: {e}"));
        assert!(revision_token(&saved["revision"]), "{saved}");
    }

    // ------------------------------------------------------------------
    // The core local guard matrix.
    // ------------------------------------------------------------------

    #[test]
    fn agent_settings_command_guarded_lifecycle() {
        let mut fx = Fx::start();
        // No master is REGISTERED at fixture start and none owns a
        // session at any point below — the whole flow must work anyway.
        assert!(
            fx.store().agent_opt(MASTER).unwrap().is_none(),
            "fixture unexpectedly has a master"
        );

        // Caller classes. Ordinary workers and the reserved master
        // alias (registered through the store — `agent_register`
        // refuses it, so no socket ancestor can ever derive it; a row
        // in that impossible state must still be fail-closed).
        fx.register(WORKER, "worker");
        fx.register(OTHER, "worker");
        fx.store()
            .register_agent(&store::NewAgent {
                alias: MASTER,
                // A pi master whose stored launch model is the
                // off-scope fixture model — `master_send_preflight`'s
                // deterministic configuration verdict, never a live
                // endpoint (the refusing provider env stays).
                provider: "pi",
                endpoint_kind: "inbox",
                role: "pm",
                cwd: fx.pm().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: Some(&json!({"model": PI_MODEL}).to_string()),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        // A member-bound app assistant gets NO agent row: an asserted
        // alias that exists only in `assistants` derives no agent
        // identity and is Unproven.
        fx.store()
            .assistant_bind("local", "sub-c1586", "assist_c1586", ASSIST, Some("rev0"))
            .unwrap();

        // The App-scope contrast needs a real installation + context +
        // conversation in the store, seeded through the same public
        // calls production uses — never an injected turn-scope flag.
        let install = fx.op(
            "app_workspace_install",
            json!({"source": concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/apps/crm")}),
        )["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let app_context = fx
            .store()
            .app_context_create(
                &install,
                &store::app_contexts::ContextConfig::new("C1586", BTreeMap::new()).unwrap(),
                "req-c1586",
            )
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let app_conversation = fx.op(
            "conversation_create",
            json!({"alias": MASTER, "install_id": install, "context_id": app_context}),
        )["conversation"]["id"]
            .as_str()
            .unwrap()
            .to_string();

        seed_pi_model(&fx);

        let snap = fx.snapshot();
        let held = fx.hold(&snap);
        let rev = snap["revision"].clone();

        // Classes that reach nothing — every op including the petition.
        let refuse_all = |fx: &Fx, who: Asserted, label: &str| {
            for (operation, expected, payload) in [
                ("snapshot", Value::Null, json!({})),
                ("history", Value::Null, json!({})),
                ("harness.list", Value::Null, json!({})),
                ("model.list", Value::Null, json!({})),
                ("master.status", Value::Null, json!({})),
                ("master.resume", Value::Null, json!({})),
                (
                    "model.scope",
                    rev.clone(),
                    json!({"id": PI_MODEL, "scope": "master"}),
                ),
                ("apply", rev.clone(), json!({"config": held.config.clone()})),
                ("role.set", rev.clone(), json!({"role": "dev", "model": ""})),
                (
                    "pin.put",
                    rev.clone(),
                    json!({"pkg": "@scope/ext", "pinned": "1.0.0"}),
                ),
                (
                    "profile.preview",
                    rev.clone(),
                    json!({"profile": "none", "harness": "pi"}),
                ),
                ("catalog.refresh", rev.clone(), json!({"provider": "pi"})),
                (
                    "grant.request",
                    Value::Null,
                    json!({
                        "operation": "pin.put",
                        "expected_revision": rev.clone(),
                        "payload": {"pkg": "x", "pinned": "1"},
                        "reason": "r",
                    }),
                ),
                ("grant.list", Value::Null, json!({})),
                (
                    "grant.allow",
                    Value::Null,
                    json!({"request_id": "req-none"}),
                ),
                (
                    "grant.reject",
                    Value::Null,
                    json!({"request_id": "req-none"}),
                ),
                ("grant.revoke", Value::Null, json!({"grant_id": "g-none"})),
            ] {
                let err = fx.cmd_err(who.clone(), operation, expected, payload);
                assert!(
                    !err.to_string().to_lowercase().contains("unknown method"),
                    "{label} hit an unknown-method dispatch on {operation}: {err}"
                );
            }
            assert_held(fx, &held);
        };

        refuse_all(&fx, Asserted::Unproven, "unproven");
        refuse_all(&fx, Asserted::Agent(GHOST.into()), "unregistered alias");
        // The master caller is NOT refused-all: production and the seam
        // agree on `Agent` subject classification. HERE no turn is
        // running, so `master_turn_scope` is Unresolved and fail-closed
        // `master_policy` refuses every op itself — proving a master
        // with no active Home turn holds nothing. The Home-scope grant
        // matrix runs after the seeded running turn below.
        let master = Asserted::Agent(MASTER.into());
        for op in [
            ("snapshot", Value::Null, json!({})),
            ("master.status", Value::Null, json!({})),
            ("master.resume", Value::Null, json!({})),
            (
                "model.scope",
                rev.clone(),
                json!({"id": PI_MODEL, "scope": "master"}),
            ),
            (
                "grant.allow",
                Value::Null,
                json!({"request_id": "req-none"}),
            ),
            ("grant.revoke", Value::Null, json!({"grant_id": "g-none"})),
        ] {
            fx.cmd_err(master.clone(), op.0, op.1, op.2);
        }
        refuse_all(&fx, Asserted::Agent(ASSIST.into()), "member assistant");

        // An ordinary worker: everything refused except its petition ops.
        let worker = Asserted::Agent(WORKER.into());
        let other = Asserted::Agent(OTHER.into());
        for (operation, expected, payload) in [
            ("snapshot", Value::Null, json!({})),
            ("history", Value::Null, json!({})),
            ("master.status", Value::Null, json!({})),
            ("master.resume", Value::Null, json!({})),
            ("harness.list", Value::Null, json!({})),
            ("harness.show", Value::Null, json!({"id": "pi"})),
            ("provider.list", Value::Null, json!({})),
            ("provider.show", Value::Null, json!({"id": "pi"})),
            ("model.list", Value::Null, json!({})),
            ("model.show", Value::Null, json!({"id": PI_MODEL})),
            ("role.list", Value::Null, json!({})),
            ("profile.list", Value::Null, json!({})),
            ("profile.show", Value::Null, json!({"id": "none"})),
            ("pin.list", Value::Null, json!({})),
            (
                "model.scope",
                rev.clone(),
                json!({"id": PI_MODEL, "scope": "master"}),
            ),
            (
                "harness.put",
                rev.clone(),
                json!({"id": "h", "label": "h", "cli": "pi", "note": ""}),
            ),
            ("harness.remove", rev.clone(), json!({"id": "h"})),
            (
                "provider.put",
                rev.clone(),
                json!({"id":"p","label":"p","kind":"native CLI","pin":"","gate":false,"paid":false,"note":""}),
            ),
            ("provider.remove", rev.clone(), json!({"id": "p"})),
            (
                "model.put",
                rev.clone(),
                json!({"id": "m", "provider": "pi", "ctx": "", "out": ""}),
            ),
            (
                "model.rename",
                rev.clone(),
                json!({"id": PI_MODEL, "to": "renamed"}),
            ),
            ("model.remove", rev.clone(), json!({"id": PI_MODEL})),
            ("apply", rev.clone(), json!({"config": held.config.clone()})),
            (
                "role.set",
                rev.clone(),
                json!({"role": "dev", "model": PI_MODEL}),
            ),
            ("role.clear", rev.clone(), json!({"role": "dev"})),
            (
                "profile.put",
                rev.clone(),
                json!({"id": "p1", "role": "dev", "model": "inherit", "job": "j"}),
            ),
            (
                "profile.rename",
                rev.clone(),
                json!({"id": "p1", "to": "p2"}),
            ),
            ("profile.remove", rev.clone(), json!({"id": "p1"})),
            ("profile.sync", rev.clone(), json!({"profile": null})),
            (
                "profile.preview",
                rev.clone(),
                json!({"profile": "none", "harness": "pi"}),
            ),
            (
                "catalog.import",
                rev.clone(),
                json!({"provider": "pi", "models": []}),
            ),
            ("catalog.refresh", rev.clone(), json!({"provider": "pi"})),
            (
                "pin.put",
                rev.clone(),
                json!({"pkg": "@scope/ext", "pinned": "1.0.0"}),
            ),
            ("pin.remove", rev.clone(), json!({"pkg": "@scope/ext"})),
            ("grant.allow", Value::Null, json!({"request_id": "req-x"})),
            ("grant.reject", Value::Null, json!({"request_id": "req-x"})),
            ("grant.revoke", Value::Null, json!({"grant_id": "g-x"})),
        ] {
            fx.cmd_err(worker.clone(), operation, expected, payload);
        }
        assert_held(&fx, &held);

        // Forged authority: identity fields on outer params and inside
        // the payload refuse for every caller class — the operator too.
        for &field in FORGED_FIELDS {
            for who in [
                Asserted::Operator,
                Asserted::Agent(WORKER.into()),
                Asserted::Unproven,
            ] {
                let mut params = json!({
                    "operation": "snapshot",
                    "expected_revision": Value::Null,
                    "payload": {},
                });
                params[field] = json!("forged");
                let err = fx
                    .rpc(who.clone(), CMD, params)
                    .expect_err(&format!("outer field {field} was not refused"));
                assert!(
                    !err.to_string().to_lowercase().contains("unknown method"),
                    "outer {field}: {err}"
                );
                let mut params = json!({
                    "operation": "grant.request",
                    "expected_revision": Value::Null,
                    "payload": {
                        "operation": "pin.put",
                        "expected_revision": rev.clone(),
                        "payload": {"pkg": "x", "pinned": "1"},
                        "reason": "r",
                    },
                });
                params["payload"][field] = json!("forged");
                fx.rpc(who, CMD, params)
                    .expect_err(&format!("payload field {field} was not refused"));
            }
        }
        // Authority-shaped request fields refused on grant.request —
        // every name outside the request schema. (`payload_sha256` and
        // `grant_id` are schema-legal payload keys the request ignores:
        // the daemon computes the digest itself, so those live in the
        // intended-payload refusal list below instead.)
        for field in ["agent", "target", "digest", "as", "by", "caller"] {
            let mut payload = json!({
                "operation": "pin.put",
                "expected_revision": rev.clone(),
                "payload": {"pkg": "x", "pinned": "1"},
                "reason": "r",
            });
            payload[field] = json!("forged");
            fx.cmd_err(worker.clone(), "grant.request", Value::Null, payload);
        }
        // The same fields inside the INTENDED payload refuse — a grant
        // minted for a forged body would carry the forgery into
        // authority, so the request never stores it.
        for field in [
            "agent",
            "grant_id",
            "payload_sha256",
            "cli_envelope",
            "by",
            "operator",
        ] {
            let mut payload = json!({
                "operation": "pin.put",
                "expected_revision": rev.clone(),
                "payload": {"pkg": "x", "pinned": "1"},
                "reason": "r",
            });
            payload["payload"][field] = json!("forged");
            fx.cmd_err(worker.clone(), "grant.request", Value::Null, payload);
        }
        // A request naming an unknown operation, and one that omits the
        // observed revision for a revision-bound op, refuse too.
        fx.cmd_err(
            worker.clone(),
            "grant.request",
            Value::Null,
            json!({
                "operation": "not.an.op",
                "expected_revision": Value::Null,
                "payload": {},
                "reason": "r",
            }),
        );
        fx.cmd_err(
            worker.clone(),
            "grant.request",
            Value::Null,
            json!({
                "operation": "pin.put",
                "payload": {"pkg": "x", "pinned": "1"},
                "reason": "r",
            }),
        );
        // Grant issuance is never delegable: a petition FOR a decision op
        // refuses before it is stored.
        for operation in ["grant.allow", "grant.reject", "grant.revoke"] {
            fx.cmd_err(
                worker.clone(),
                "grant.request",
                Value::Null,
                json!({
                    "operation": operation,
                    "expected_revision": Value::Null,
                    "payload": {"request_id": "x"},
                    "reason": "delegation attempt",
                }),
            );
        }
        assert_held(&fx, &held);
        assert!(
            fx.grants().get("requests").is_none()
                || fx.grants()["requests"].as_array().unwrap().is_empty(),
            "refused grant.request calls wrote request rows"
        );

        // -- the approved managing-agent lifecycle (no master needed) --
        // Petition first: the stored row is pending and binds the
        // derived subject (kind agent, alias + generation + uid), the
        // operation, the verbatim payload and the observed revision.
        let repair = json!({"id": PI_MODEL, "scope": "master"});
        let request_id = petition(
            &fx,
            &worker,
            "model.scope",
            &rev,
            repair.clone(),
            "master is configuration-unlaunchable on this model",
        );
        let persisted = fx.grants();
        let row = persisted["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(request_id.as_str()))
            .unwrap_or_else(|| panic!("request {request_id} not persisted: {persisted}"));
        assert_eq!(row["subject"]["kind"].as_str(), Some("agent"), "{row}");
        assert_eq!(row["subject"]["alias"].as_str(), Some(WORKER), "{row}");
        assert!(
            row["subject"]["generation"].is_string() || row["subject"]["generation"].is_number(),
            "request bound no registration generation: {row}"
        );
        assert!(
            row["subject"].get("uid").is_some(),
            "request bound no uid: {row}"
        );
        assert_eq!(row["operation"].as_str(), Some("model.scope"), "{row}");
        assert_eq!(row["expected_revision"], rev, "{row}");
        assert_eq!(
            row["payload"], repair,
            "request stored no bound payload: {row}"
        );
        assert_eq!(row["status"].as_str(), Some("pending"), "{row}");
        let grant_id = allow(&fx, &request_id);

        // The worker sees only its own rows; the operator sees all.
        let mine = fx
            .cmd(worker.clone(), "grant.list", Value::Null, json!({}))
            .expect("worker grant.list");
        assert_eq!(
            mine["requests"].as_array().unwrap().len(),
            1,
            "worker grant.list is not scoped to its own rows: {mine}"
        );
        let theirs = fx
            .cmd(other.clone(), "grant.list", Value::Null, json!({}))
            .expect("other worker grant.list");
        assert!(
            theirs["requests"].as_array().unwrap().is_empty()
                && theirs["grants"].as_array().unwrap().is_empty(),
            "foreign worker read another agent's requests: {theirs}"
        );
        let all = fx
            .cmd(Asserted::Operator, "grant.list", Value::Null, json!({}))
            .expect("operator grant.list");
        assert_eq!(all["requests"].as_array().unwrap().len(), 1, "{all}");

        // Decisions stay operator-owned.
        for who in [worker.clone(), other.clone(), Asserted::Unproven] {
            fx.cmd_err(
                who,
                "grant.allow",
                Value::Null,
                json!({"request_id": request_id.as_str()}),
            );
        }
        fx.cmd_err(
            Asserted::Operator,
            "grant.allow",
            Value::Null,
            json!({"request_id": "req-nonexistent"}),
        );
        // The persisted grant carries the decision-time binding.
        let persisted = fx.grants();
        let grown = persisted["grants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"].as_str() == Some(grant_id.as_str()))
            .unwrap_or_else(|| panic!("grant {grant_id} not persisted: {persisted}"));
        assert_eq!(grown["request_id"].as_str(), Some(request_id.as_str()));
        assert_eq!(grown["subject"]["alias"].as_str(), Some(WORKER));
        assert_eq!(grown["operation"].as_str(), Some("model.scope"));
        assert_eq!(grown["org"].as_str(), Some("local"));
        assert_eq!(grown["revoked"], json!(false));
        assert!(
            grown["used_at"].is_null(),
            "fresh grant already used: {grown}"
        );
        let expires = grown["expires_at"].as_i64().expect("expires_at");
        assert!(
            expires > now() && expires <= now() + 901,
            "grant expiry is not decision+900s: {grown}"
        );
        assert!(
            grown["payload_sha256"]
                .as_str()
                .is_some_and(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit())),
            "bound payload digest: {grown}"
        );

        // -- send preflight while still disallowed (CAD-1586): the
        // registered master's stored model is off-scope, so every send
        // entry refuses `master_unlaunchable` BEFORE the durable
        // enqueue — the queue and messages stay empty.
        let inbox = fx.op("message_list", json!({"to": MASTER}));
        assert!(
            inbox["messages"].as_array().unwrap().is_empty(),
            "fixture master already has mail: {inbox}"
        );
        let send = json!({
            "alias": MASTER,
            "text": "unlaunchable preflight probe",
            "message": "c1586-send-1",
        });
        let err = fx
            .rpc(Asserted::Operator, "agent_send", send.clone())
            .expect_err("send to a configuration-unlaunchable master queued");
        assert_eq!(
            err.code(),
            Some("master_unlaunchable"),
            "send refusal is not master_unlaunchable: {err}"
        );
        let err = fx
            .rpc(worker.clone(), "agent_send", send.clone())
            .expect_err("worker send to an unlaunchable master queued");
        assert_eq!(err.code(), Some("master_unlaunchable"), "{err}");
        // The remote send funnel shares the same preflight — a verified
        // owner's `message_send` gets the same typed refusal over HTTP.
        let owner_env = fx.envelope(
            "operator",
            "user",
            "owner",
            &["cli.read", "cli.write", "settings.read", "settings.write"],
        );
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            "/api/cli/message_send",
            &send,
            Some(&Fx::bearer(&owner_env)),
        );
        assert_eq!(
            status, 400,
            "remote send to an unlaunchable master: {status} {text}"
        );
        assert_eq!(
            parse_json(&text)["error"]["code"].as_str(),
            Some("master_unlaunchable"),
            "remote send refusal lost the code: {status} {text}"
        );
        let inbox = fx.op("message_list", json!({"to": MASTER}));
        assert!(
            inbox["messages"].as_array().unwrap().is_empty(),
            "a refused send still queued: {inbox}"
        );
        assert_held(&fx, &held);

        // The exact approved repair succeeds and returns the post-change
        // snapshot the caller can verify — while no master exists.
        let repaired = fx
            .cmd(
                worker.clone(),
                "model.scope",
                rev.clone(),
                json!({"id": PI_MODEL, "scope": "master", "grant_id": grant_id.as_str()}),
            )
            .unwrap_or_else(|e| panic!("granted model.scope repair: {e}"));
        assert!(revision_token(&repaired["revision"]), "{repaired}");
        assert_ne!(repaired["revision"], rev);
        assert_eq!(
            repaired["config"]["scope"][PI_MODEL].as_str(),
            Some("master"),
            "repair not in the post-change snapshot: {repaired}"
        );
        assert_eq!(fx.snapshot()["config"], repaired["config"]);
        // Replay: single-use.
        fx.cmd_err(
            worker.clone(),
            "model.scope",
            repaired["revision"].clone(),
            json!({"id": PI_MODEL, "scope": "master", "grant_id": grant_id.as_str()}),
        );

        // The repaired scope makes the master stopped-but-launchable:
        // the same sends now queue (a live launch stays refused by the
        // provider fixture — queueing is the preflight's whole job).
        let sent = fx
            .rpc(Asserted::Operator, "agent_send", send.clone())
            .unwrap_or_else(|e| panic!("send to a launchable master refused: {e}"));
        assert!(sent["message"].is_string(), "no queued receipt: {sent}");
        let inbox = fx.op("message_list", json!({"to": MASTER}));
        assert_eq!(
            inbox["messages"].as_array().unwrap().len(),
            1,
            "post-repair send did not queue: {inbox}"
        );

        // `master.status` rides the same grant machinery — the exact op
        // the repair workflow diagnoses with. It carries no document
        // CAS: the request binds a null expected_revision.
        let (_, status_grant) = granted(
            &fx,
            &worker,
            "master.status",
            &Value::Null,
            json!({}),
            "diagnose the down master",
        );
        let diagnosed = fx
            .cmd(
                worker.clone(),
                "master.status",
                Value::Null,
                json!({"grant_id": status_grant.as_str()}),
            )
            .unwrap_or_else(|e| panic!("granted master.status: {e}"));
        assert!(diagnosed.is_object(), "master.status answer: {diagnosed}");
        assert_eq!(
            fx.snapshot()["revision"],
            repaired["revision"],
            "master.status wrote settings"
        );

        // `master.resume` — the bounded recovery op — rides the same
        // exact-grant machinery: no expected_revision, an empty
        // approved payload, no GrantExec bypass. The fences inside
        // `try_resume` stay authoritative: this fixture keeps every
        // provider refusing, so the op reports its honest verdict
        // (`attention`, not a claimed start) or its own refusal —
        // either way the GRANT gate passed and was consumed, and no
        // settings moved.
        let held = fx.hold(&fx.snapshot());
        // A non-null expected_revision is refused outright, grant or
        // not — the op never touches the document CAS.
        fx.cmd_err(
            Asserted::Operator,
            "master.resume",
            repaired["revision"].clone(),
            json!({}),
        );
        let (_, resume_grant) = granted(
            &fx,
            &worker,
            "master.resume",
            &Value::Null,
            json!({}),
            "restart the repaired master",
        );
        // The wrong operation and the wrong caller refuse WITHOUT
        // consuming the grant.
        fx.cmd_err(
            worker.clone(),
            "master.status",
            Value::Null,
            json!({"grant_id": resume_grant.as_str()}),
        );
        fx.cmd_err(
            other.clone(),
            "master.resume",
            Value::Null,
            json!({"grant_id": resume_grant.as_str()}),
        );
        let resumed = fx.cmd(
            worker.clone(),
            "master.resume",
            Value::Null,
            json!({"grant_id": resume_grant.as_str()}),
        );
        let row = fx.grants()["grants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"].as_str() == Some(resume_grant.as_str()))
            .cloned()
            .unwrap();
        assert!(
            !row["used_at"].is_null(),
            "the granted master.resume never reached the op: {row}"
        );
        if let Ok(answer) = resumed {
            assert_eq!(answer["alias"].as_str(), Some(MASTER), "{answer}");
            assert!(
                matches!(
                    answer["state"].as_str(),
                    Some("starting") | Some("attention")
                ),
                "master.resume claimed an unverifiable state: {answer}"
            );
        }
        // Replay refuses — single-use holds on the recovery op too.
        fx.cmd_err(
            worker.clone(),
            "master.resume",
            Value::Null,
            json!({"grant_id": resume_grant.as_str()}),
        );
        assert_held(&fx, &held);

        // -- exact-operation binding: every variant refuses BEFORE mutation --
        let rev2 = fx.snapshot()["revision"].clone();
        let (_, pin_grant) = granted(
            &fx,
            &worker,
            "pin.put",
            &rev2,
            json!({"pkg": "@scope/ext", "pinned": "1.4.2"}),
            "pin an extension",
        );
        let held = fx.hold(&fx.snapshot());
        let pin = |grant: &str| json!({"pkg": "@scope/ext", "pinned": "1.4.2", "grant_id": grant});
        // wrong operation (the grant is for pin.put, not model.scope
        // or master.status)
        fx.cmd_err(
            worker.clone(),
            "model.scope",
            rev2.clone(),
            json!({"id": PI_MODEL, "scope": "worker", "grant_id": pin_grant.as_str()}),
        );
        fx.cmd_err(
            worker.clone(),
            "master.status",
            Value::Null,
            json!({"grant_id": pin_grant.as_str()}),
        );
        // wrong resource id inside the payload
        fx.cmd_err(
            worker.clone(),
            "pin.put",
            rev2.clone(),
            json!({"pkg": "@scope/other", "pinned": "1.4.2", "grant_id": pin_grant.as_str()}),
        );
        // changed post-approval field value (a replaced file body)
        fx.cmd_err(
            worker.clone(),
            "pin.put",
            rev2.clone(),
            json!({"pkg": "@scope/ext", "pinned": "9.9.9", "grant_id": pin_grant.as_str()}),
        );
        // wrong expected_revision at use time
        fx.cmd_err(
            worker.clone(),
            "pin.put",
            json!("0:deadbeefcafe"),
            pin(pin_grant.as_str()),
        );
        // a different caller holding the id
        fx.cmd_err(
            other.clone(),
            "pin.put",
            rev2.clone(),
            pin(pin_grant.as_str()),
        );
        // forged ids — including the consumed repair grant's id
        for bad in ["g-forged", "", grant_id.as_str()] {
            fx.cmd_err(worker.clone(), "pin.put", rev2.clone(), pin(bad));
        }
        // missing grant_id — an ordinary agent with no grant
        fx.cmd_err(
            worker.clone(),
            "pin.put",
            rev2.clone(),
            json!({"pkg": "@scope/ext", "pinned": "1.4.2"}),
        );
        // an identity field inside the granted payload still refuses
        let mut forged = pin(pin_grant.as_str());
        forged["by"] = json!("operator");
        fx.cmd_err(worker.clone(), "pin.put", rev2.clone(), forged);
        assert_held(&fx, &held);
        // Refused authorization attempts did not consume the grant.
        let pinned = fx
            .cmd(
                worker.clone(),
                "pin.put",
                rev2.clone(),
                pin(pin_grant.as_str()),
            )
            .unwrap_or_else(|e| panic!("legitimate grant use after refused attempts: {e}"));
        assert!(revision_token(&pinned["revision"]), "{pinned}");
        let grants_after = fx.grants();
        let row = grants_after["grants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["id"].as_str() == Some(pin_grant.as_str()))
            .unwrap();
        assert!(!row["used_at"].is_null(), "used grant not recorded: {row}");
        // Replay: single-use.
        fx.cmd_err(
            worker.clone(),
            "pin.put",
            pinned["revision"].clone(),
            pin(pin_grant.as_str()),
        );

        // -- expired grant: the persisted expires_at is the authority --
        let rev3 = fx.snapshot()["revision"].clone();
        let (_, expired_id) = granted(
            &fx,
            &worker,
            "role.set",
            &rev3,
            json!({"role": "dev", "model": PI_MODEL}),
            "expiry case",
        );
        let held = fx.hold(&fx.snapshot());
        fx.rewrite_grants(|doc| {
            for g in doc["grants"].as_array_mut().unwrap() {
                if g["id"].as_str() == Some(expired_id.as_str()) {
                    g["expires_at"] = json!(now() - 10);
                }
            }
        });
        fx.cmd_err(
            worker.clone(),
            "role.set",
            rev3.clone(),
            json!({"role": "dev", "model": PI_MODEL, "grant_id": expired_id.as_str()}),
        );
        assert_held(&fx, &held);

        // -- revoked grant --
        let rev4 = fx.snapshot()["revision"].clone();
        let (_, revoked_id) = granted(
            &fx,
            &worker,
            "role.clear",
            &rev4,
            json!({"role": "dev"}),
            "revocation case",
        );
        for who in [worker.clone(), other.clone(), Asserted::Unproven] {
            fx.cmd_err(
                who,
                "grant.revoke",
                Value::Null,
                json!({"grant_id": revoked_id.as_str()}),
            );
        }
        fx.cmd_err(
            Asserted::Operator,
            "grant.revoke",
            Value::Null,
            json!({"grant_id": "g-nonexistent"}),
        );
        let gone = fx
            .cmd(
                Asserted::Operator,
                "grant.revoke",
                Value::Null,
                json!({"grant_id": revoked_id.as_str()}),
            )
            .expect("operator revoke");
        assert_eq!(gone["status"].as_str(), Some("revoked"), "{gone}");
        assert_eq!(gone["grant_id"].as_str(), Some(revoked_id.as_str()));
        let persisted = fx.grants();
        assert!(
            persisted["grants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["id"].as_str() == Some(revoked_id.as_str())
                    && g["revoked"] == json!(true)),
            "revocation not persisted: {persisted}"
        );
        fx.cmd_err(
            worker.clone(),
            "role.clear",
            rev4.clone(),
            json!({"role": "dev", "grant_id": revoked_id.as_str()}),
        );
        assert_held(&fx, &held);

        // -- rejected request grants nothing --
        let request = fx
            .cmd(
                worker.clone(),
                "grant.request",
                Value::Null,
                json!({
                    "operation": "harness.remove",
                    "expected_revision": rev4.clone(),
                    "payload": {"id": "pi"},
                    "reason": "rejection case",
                }),
            )
            .unwrap();
        let rejected_id = request["request_id"].as_str().unwrap().to_string();
        let denied = fx
            .cmd(
                Asserted::Operator,
                "grant.reject",
                Value::Null,
                json!({"request_id": rejected_id.as_str()}),
            )
            .expect("operator reject");
        assert_eq!(denied["status"].as_str(), Some("rejected"), "{denied}");
        assert_eq!(denied["request_id"].as_str(), Some(rejected_id.as_str()));
        fx.cmd_err(
            Asserted::Operator,
            "grant.allow",
            Value::Null,
            json!({"request_id": rejected_id.as_str()}),
        );
        let persisted = fx.grants();
        assert!(
            persisted["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"].as_str() == Some(rejected_id.as_str())
                    && r["status"].as_str() == Some("rejected")),
            "rejection not persisted: {persisted}"
        );

        // -- the granted file-body path: profile.put binds job bytes --
        let rev5 = fx.snapshot()["revision"].clone();
        let job_body = "line one of the approved job\nline two\n";
        let approved_job = json!({
            "id": "c1586-approved-profile",
            "role": "worker",
            "model": "inherit",
            "job": job_body,
        });
        let (_, gid) = granted(
            &fx,
            &worker,
            "profile.put",
            &rev5,
            approved_job.clone(),
            "approved profile body",
        );
        let held = fx.hold(&fx.snapshot());
        // A file replaced after approval — any job-byte change refuses.
        let mut swapped = approved_job.clone();
        swapped["job"] = json!("line one of the approved job\nline two CHANGED\n");
        swapped["grant_id"] = json!(gid.as_str());
        fx.cmd_err(worker.clone(), "profile.put", rev5.clone(), swapped);
        let mut retarget = approved_job.clone();
        retarget["id"] = json!("c1586-other-profile");
        retarget["grant_id"] = json!(gid.as_str());
        fx.cmd_err(worker.clone(), "profile.put", rev5.clone(), retarget);
        assert_held(&fx, &held);
        let mut use_job = approved_job.clone();
        use_job["grant_id"] = json!(gid.as_str());
        let saved = fx
            .cmd(worker.clone(), "profile.put", rev5.clone(), use_job)
            .unwrap_or_else(|e| panic!("granted profile.put: {e}"));
        assert!(
            saved["config"]["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"].as_str() == Some("c1586-approved-profile")),
            "post-change snapshot lacks the profile: {saved}"
        );

        // -- CAS: a stale token is a conflict and commits nothing --
        let before = fx.hold(&fx.snapshot());
        let moved = fx
            .cmd(
                Asserted::Operator,
                "harness.put",
                before.revision.clone(),
                json!({"id": "devin", "label": "h", "cli": "devin", "note": ""}),
            )
            .unwrap();
        assert_ne!(moved["revision"], before.revision);
        let err = fx.cmd_err(
            Asserted::Operator,
            "harness.put",
            before.revision.clone(),
            json!({"id": "claude", "label": "h2", "cli": "claude", "note": ""}),
        );
        assert_eq!(
            err.kind(),
            "conflict",
            "stale operator write was not a revision conflict: {err}"
        );
        // A granted agent cannot skip CAS either: the grant binds the
        // revision the agent observed, so a document moved under it
        // refuses before mutation.
        let cas_rev = moved["revision"].clone();
        let (_, cas_grant) = granted(
            &fx,
            &worker,
            "pin.remove",
            &cas_rev,
            json!({"pkg": "@scope/ext"}),
            "cas case",
        );
        let moved_again = fx
            .cmd(
                Asserted::Operator,
                "harness.put",
                cas_rev.clone(),
                json!({"id": "claude", "label": "h3", "cli": "claude", "note": ""}),
            )
            .unwrap();
        assert_ne!(moved_again["revision"], cas_rev);
        fx.cmd_err(
            worker.clone(),
            "pin.remove",
            cas_rev.clone(),
            json!({"pkg": "@scope/ext", "grant_id": cas_grant.as_str()}),
        );
        let now_held = fx.hold(&fx.snapshot());
        assert_eq!(
            now_held.revision, moved_again["revision"],
            "the refused CAS write still committed"
        );

        // -- durability: the grant store survives a daemon restart --
        let (_, durable_grant) = granted(
            &fx,
            &worker,
            "snapshot",
            &Value::Null,
            json!({}),
            "post-restart read grant",
        );
        fx.restart_daemon();
        let listed = fx
            .cmd(Asserted::Operator, "grant.list", Value::Null, json!({}))
            .expect("grant.list after restart");
        assert!(
            listed["requests"].as_array().unwrap().len() >= 7,
            "restarted daemon lost grant requests: {listed}"
        );
        let row = fx.grants();
        assert!(
            row["requests"].as_array().unwrap().len() >= 7
                && row["grants"].as_array().unwrap().len() >= 6,
            "settings-grants.json lost rows across restart: {row}"
        );
        // A grant minted before the restart still authorizes after it —
        // the file is the authority, not daemon memory.
        let after = fx
            .cmd(
                worker.clone(),
                "snapshot",
                Value::Null,
                json!({"grant_id": durable_grant.as_str()}),
            )
            .unwrap_or_else(|e| panic!("pre-restart grant unusable after restart: {e}"));
        assert!(revision_token(&after["revision"]), "{after}");

        // -- the master caller through master_policy's real scope --
        // `master_turn_scope` reads the running message's stored
        // conversation, so the fixture seeds a real turn: the queued
        // post-repair send becomes the master's RUNNING home-thread
        // turn via the store's own claim path (never a scope flag).
        // While Home: `agent_settings_command` is in MASTER_ALLOWED,
        // so the master caller reaches the settings grant gate —
        // ungranted ops refuse there (the petition/list verbs pass
        // the gate and run), and an exact grant executes.
        fx.db_execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
            &["c1586-send-1"],
        );
        fx.store()
            .mark_running("c1586-send-1", "c1586-home-turn")
            .unwrap();
        let master = Asserted::Agent(MASTER.into());
        for op in [
            ("snapshot", Value::Null, json!({})),
            ("master.status", Value::Null, json!({})),
            ("master.resume", Value::Null, json!({})),
            (
                "model.scope",
                fx.snapshot()["revision"].clone(),
                json!({"id": PI_MODEL, "scope": "master"}),
            ),
            (
                "grant.allow",
                Value::Null,
                json!({"request_id": "req-none"}),
            ),
            ("grant.revoke", Value::Null, json!({"grant_id": "g-none"})),
        ] {
            fx.cmd_err(master.clone(), op.0, op.1, op.2);
        }
        // Under Home the master holds the same petition authority as
        // any agent subject: it may request, and an operator decision
        // mints a grant bound to the master alias that runs that op
        // only — never a self-issued decision.
        let (_, master_grant) = granted(
            &fx,
            &master,
            "snapshot",
            &Value::Null,
            json!({}),
            "master reads its own settings snapshot",
        );
        let snap = fx
            .cmd(
                master.clone(),
                "snapshot",
                Value::Null,
                json!({"grant_id": master_grant.as_str()}),
            )
            .unwrap_or_else(|e| panic!("master's own granted read: {e}"));
        assert!(revision_token(&snap["revision"]), "{snap}");

        // An App conversation's running turn narrows the master to the
        // scoped app verbs: a minted grant can never widen that — the
        // refusal is `master_policy`'s, before the settings handler.
        let app_send = fx.op(
            "thread_send",
            json!({
                "alias": MASTER,
                "text": "app turn",
                "message": "c1586-app-1",
                "app": {"install_id": install, "context_id": app_context},
                "conversation": app_conversation,
            }),
        );
        let app_msg = app_send["message"].as_str().unwrap().to_string();
        fx.db_execute(
            "UPDATE messages SET state='completed' WHERE id='c1586-send-1'",
            &[],
        );
        fx.db_execute(
            "UPDATE messages SET state='submitting', started=1.0 WHERE id=?",
            &[app_msg.as_str()],
        );
        fx.store().mark_running(&app_msg, "c1586-app-turn").unwrap();
        // The ungranted petition is refused identically to every op.
        fx.cmd_err(
            master.clone(),
            "grant.request",
            Value::Null,
            json!({
                "operation": "snapshot",
                "expected_revision": Value::Null,
                "payload": {},
                "reason": "r",
            }),
        );
        // A grant minted under Home is authority the App turn never
        // reaches: bound to the master's identity, still refused at
        // the policy gate before `agent_settings_command` runs.
        let (_, status_grant) = granted(
            &fx,
            &worker,
            "master.status",
            &Value::Null,
            json!({}),
            "minted for the master caller",
        );
        fx.cmd_err(
            master.clone(),
            "master.status",
            Value::Null,
            json!({"grant_id": status_grant.as_str()}),
        );
        // Restore Home so nothing below inherits the narrowed scope.
        fx.db_execute(
            "UPDATE messages SET state='completed' WHERE id=?",
            &[app_msg.as_str()],
        );

        // -- the preserved UI surface stays operator-gated and agrees --
        let get = fx.op(GET, json!({}));
        assert_eq!(get["revision"], fx.snapshot()["revision"]);
        for who in [Asserted::Agent(WORKER.into()), Asserted::Unproven] {
            fx.rpc(who.clone(), GET, json!({}))
                .expect_err("non-operator read of the legacy surface");
            fx.rpc(
                who,
                SET,
                json!({
                    "expected_revision": get["revision"].clone(),
                    "config": get["config"].clone(),
                }),
            )
            .expect_err("non-operator write of the legacy surface");
        }
        let session = fx.operator_session();
        let (status, reply, _) = fx.local_post(
            &Asserted::Operator,
            ROUTE,
            &json!({
                "operation": "model.scope",
                "expected_revision": get["revision"].clone(),
                "payload": {"id": PI_MODEL, "scope": "off"},
            }),
            Some(&session),
        );
        assert!(
            status >= 400,
            "the legacy board route accepted the command envelope: {status} {reply}"
        );
        let (status, reply, _) = fx.local_get(
            &Asserted::Agent(WORKER.into()),
            ROUTE,
            Some(&fx.operator_session()),
        );
        assert!(
            matches!(status, 401 | 403),
            "agent-asserted board GET: {status} {reply}"
        );
    }

    // ------------------------------------------------------------------
    // The remote-envelope peer: `POST /api/cli/settings_agents` and its
    // daemon re-verify `cli_settings_command`.
    // ------------------------------------------------------------------

    #[test]
    fn cli_settings_agents_envelope_guards() {
        let fx = Fx::start();
        fx.register(WORKER, "worker");
        let rev = fx.snapshot()["revision"].clone();
        let held = fx.hold(&fx.snapshot());

        // The verb exists on the public host and refuses cleanly: no
        // bearer is never a body-level error, a session cookie cannot
        // substitute, and a foreign tail never reaches dispatch.
        let session = fx.operator_session();
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "snapshot", "expected_revision": null, "payload": {}}),
            None,
        );
        assert_eq!(status, 401, "no-bearer cli call: {status} {text}");
        let (status, text, _) = fx.exchange(
            &Asserted::Operator,
            "POST",
            HOST,
            CLI_ROUTE,
            Some(&json!({"operation": "snapshot", "expected_revision": null, "payload": {}})),
            Some(&session),
            None,
        );
        assert_eq!(
            status, 401,
            "an operator session satisfied the cli bearer: {status} {text}"
        );
        let (status, _) = fx.cli_post(
            &Asserted::Operator,
            "/api/cli/settings_agents/extra",
            &json!({}),
            None,
        );
        assert_eq!(status, 404, "nested cli tail: {status}");
        assert_held(&fx, &held);

        // An owner user reaches the whole surface through the real
        // route: a read, then the same call on the daemon RPC peer the
        // board relays to.
        let owner = || {
            Fx::bearer(&fx.envelope(
                "operator",
                "user",
                "owner",
                &["settings.read", "settings.write"],
            ))
        };
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "snapshot", "expected_revision": null, "payload": {}}),
            Some(&owner()),
        );
        assert_eq!(status, 200, "owner remote snapshot: {status} {text}");
        let remote = parse_json(&text);
        assert_eq!(remote["revision"], held.revision);
        assert_eq!(remote["config"], held.config);

        let direct = fx
            .cli_cmd(
                Asserted::Operator,
                &fx.envelope(
                    "operator",
                    "user",
                    "owner",
                    &["settings.read", "settings.write"],
                ),
                "snapshot",
                Value::Null,
                json!({}),
            )
            .unwrap_or_else(|e| panic!("cli_settings_command snapshot: {e}"));
        assert_eq!(direct["revision"], held.revision);

        // jti is single-use at the route: replaying the SAME envelope
        // refuses before dispatch, with no settings effect.
        let env = fx.envelope(
            "operator",
            "user",
            "owner",
            &["settings.read", "settings.write"],
        );
        let (status, _) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "history", "expected_revision": null, "payload": {}}),
            Some(&Fx::bearer(&env)),
        );
        assert_eq!(status, 200, "first envelope use");
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "history", "expected_revision": null, "payload": {}}),
            Some(&Fx::bearer(&env)),
        );
        assert_eq!(status, 401, "replayed jti: {status} {text}");

        // Envelope-layer refusals — signature, kid, org, time, scope
        // vocabulary — all before any settings work.
        let wrong_key = Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        for (label, env) in [
            (
                "unknown kid",
                fx.sign_with(
                    &fx.key,
                    &fx.claims("operator", "user", "owner", &["settings.read"], "jti-k1"),
                    "other-kid",
                ),
            ),
            (
                "wrong signer",
                fx.sign_with(
                    &wrong_key,
                    &fx.claims("operator", "user", "owner", &["settings.read"], "jti-k2"),
                    KID,
                ),
            ),
            ("other org", {
                let mut c = fx.claims("operator", "user", "owner", &["settings.read"], "jti-k3");
                c["organization_id"] = json!("ws_other");
                fx.sign_with(&fx.key, &c, KID)
            }),
            ("expired", {
                let mut c = fx.claims("operator", "user", "owner", &["settings.read"], "jti-k4");
                c["iat"] = json!(now() - 400);
                c["exp"] = json!(now() - 100);
                fx.sign_with(&fx.key, &c, KID)
            }),
            (
                "cli-scope only",
                fx.envelope("operator", "user", "owner", &["cli.read", "cli.write"]),
            ),
        ] {
            let (status, text) = fx.cli_post(
                &Asserted::Operator,
                CLI_ROUTE,
                &json!({"operation": "snapshot", "expected_revision": null, "payload": {}}),
                Some(&Fx::bearer(&env)),
            );
            assert!(
                (400..=403).contains(&status),
                "{label} envelope reached dispatch: {status} {text}"
            );
        }
        assert_held(&fx, &held);

        // Scope separation: a settings.read envelope never mutates and
        // never petitions (grant.request is settings.write) — but it
        // does reach `master.status` and the read surface.
        let reader = || Fx::bearer(&fx.envelope("operator", "user", "owner", &["settings.read"]));
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "snapshot", "expected_revision": null, "payload": {}}),
            Some(&reader()),
        );
        assert_eq!(status, 200, "settings.read snapshot: {status} {text}");
        let (status, text) = fx.cli_post(
            &Asserted::Operator,
            CLI_ROUTE,
            &json!({"operation": "master.status", "expected_revision": null, "payload": {}}),
            Some(&reader()),
        );
        assert_eq!(status, 200, "settings.read master.status: {status} {text}");
        for operation in [
            "model.scope",
            "apply",
            "pin.put",
            "grant.request",
            "grant.allow",
            "master.resume",
        ] {
            let (status, text) = fx.cli_post(
                &Asserted::Operator,
                CLI_ROUTE,
                &json!({
                    "operation": operation,
                    "expected_revision": rev.clone(),
                    "payload": {},
                }),
                Some(&reader()),
            );
            assert!(
                (400..=403).contains(&status),
                "settings.read reached {operation}: {status} {text}"
            );
        }
        assert_held(&fx, &held);

        // The dedicated master-status verb is the same verified path —
        // strict args, settings.read scope.
        let (status, text) =
            fx.cli_post(&Asserted::Operator, CLI_MASTER, &json!({}), Some(&reader()));
        assert_eq!(status, 200, "cli master_status: {status} {text}");
        let (status, _) = fx.cli_post(
            &Asserted::Operator,
            CLI_MASTER,
            &json!({"unknown_field": "x"}),
            Some(&reader()),
        );
        assert!(
            (400..=403).contains(&status),
            "master_status accepted an authority-shaped field: {status}"
        );

        // Member and service actors are refused every settings op, on
        // the route and on the daemon RPC peer.
        for (actor, kind, role) in [
            ("users/sub-member", "user", "member"),
            ("service/bot", "service", "member"),
        ] {
            for op in ["snapshot", "grant.request", "pin.put"] {
                let env = fx.envelope(actor, kind, role, &["settings.read", "settings.write"]);
                let (status, text) = fx.cli_post(
                    &Asserted::Operator,
                    CLI_ROUTE,
                    &json!({
                        "operation": op,
                        "expected_revision": rev.clone(),
                        "payload": {},
                    }),
                    Some(&Fx::bearer(&env)),
                );
                assert!(
                    (400..=403).contains(&status),
                    "{actor} reached {op}: {status} {text}"
                );
                let env = fx.envelope(actor, kind, role, &["settings.read", "settings.write"]);
                fx.cli_cmd(Asserted::Operator, &env, op, rev.clone(), json!({}))
                    .expect_err(&format!("{actor} RPC reached {op}"));
            }
        }
        assert_held(&fx, &held);

        // A remote agents/<h> caller: petition + list only until granted.
        let remote_agent = || {
            fx.envelope(
                "agents/c1586bot",
                "agent",
                "member",
                &["settings.read", "settings.write"],
            )
        };
        for op in [
            "snapshot",
            "pin.put",
            "grant.allow",
            "master.status",
            "master.resume",
        ] {
            fx.cli_cmd(
                Asserted::Operator,
                &remote_agent(),
                op,
                rev.clone(),
                json!({}),
            )
            .expect_err(&format!("ungranted remote agent reached {op}"));
        }
        // Its own grant.list is the one surface it does reach before
        // any grant — scoped to its own (empty) rows.
        let own = fx
            .cli_cmd(
                Asserted::Operator,
                &remote_agent(),
                "grant.list",
                Value::Null,
                json!({}),
            )
            .unwrap_or_else(|e| panic!("remote grant.list: {e}"));
        assert!(
            own["requests"].as_array().unwrap().is_empty()
                && own["grants"].as_array().unwrap().is_empty(),
            "ungranted remote agent saw foreign grant rows: {own}"
        );
        let req = fx
            .cli_cmd(
                Asserted::Operator,
                &remote_agent(),
                "grant.request",
                Value::Null,
                json!({
                    "operation": "pin.put",
                    "expected_revision": rev.clone(),
                    "payload": {"pkg": "@remote/ext", "pinned": "2.0.0"},
                    "reason": "remote agent petition",
                }),
            )
            .unwrap_or_else(|e| panic!("remote grant.request: {e}"));
        let request_id = req["request_id"].as_str().unwrap().to_string();
        let persisted = fx.grants();
        let row = persisted["requests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"].as_str() == Some(request_id.as_str()))
            .unwrap_or_else(|| panic!("remote request not persisted: {persisted}"));
        assert!(
            row["subject"]["kind"].as_str() == Some("cli-agent")
                || row.to_string().contains("cli-agent")
                || row.to_string().contains("agents/c1586bot"),
            "remote request did not bind a cli-agent subject: {row}"
        );

        // A local-agent grant never satisfies a remote subject.
        let (_, local_gid) = granted(
            &fx,
            &Asserted::Agent(WORKER.into()),
            "pin.put",
            &rev,
            json!({"pkg": "@remote/ext", "pinned": "2.0.0"}),
            "local-subject case",
        );
        let held = fx.hold(&fx.snapshot());
        fx.cli_cmd(
            Asserted::Operator,
            &remote_agent(),
            "pin.put",
            rev.clone(),
            json!({"pkg": "@remote/ext", "pinned": "2.0.0", "grant_id": local_gid.as_str()}),
        )
        .expect_err("a local-agent grant satisfied a remote subject");
        assert_held(&fx, &held);

        // A local owner decision mints the hosted grant — org is the
        // configured company, and only the bound op passes.
        let allowed = fx
            .cmd(
                Asserted::Operator,
                "grant.allow",
                Value::Null,
                json!({"request_id": request_id.as_str()}),
            )
            .unwrap();
        let gid = allowed["grant_id"].as_str().unwrap().to_string();
        assert_eq!(
            allowed["bound"]["org"].as_str(),
            Some(ORG),
            "hosted grant did not bind the configured org: {allowed}"
        );
        let remote_use = fx
            .cli_cmd(
                Asserted::Operator,
                &remote_agent(),
                "pin.put",
                rev.clone(),
                json!({"pkg": "@remote/ext", "pinned": "2.0.0", "grant_id": gid.as_str()}),
            )
            .unwrap_or_else(|e| panic!("granted remote pin.put: {e}"));
        assert!(
            remote_use["config"]["pins"]
                .as_array()
                .map(|rows| rows
                    .iter()
                    .any(|p| p["pkg"].as_str() == Some("@remote/ext")))
                .unwrap_or(false),
            "remote grant did not apply: {remote_use}"
        );

        // The daemon RPC peer refuses non-relay connections outright,
        // and an envelope-free call never satisfies it.
        for who in [Asserted::Agent(WORKER.into()), Asserted::Unproven] {
            fx.cli_cmd(who, &remote_agent(), "snapshot", Value::Null, json!({}))
                .expect_err("a non-operator connection carried a cli envelope");
        }
        fx.rpc(
            Asserted::Operator,
            CLI_CMD,
            json!({"operation": "snapshot", "expected_revision": null, "payload": {}}),
        )
        .expect_err("cli_settings_command without cli_envelope");
    }
}
