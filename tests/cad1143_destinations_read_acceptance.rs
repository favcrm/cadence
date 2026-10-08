//! CAD-1143 destinations-read acceptance — written by the independent
//! acceptance author (cc13-pi-acc793), never the host implementer
//! (social-host-ca8ff), who may not edit or weaken this file.
//!
//! From the ticket: authoritative destination discovery stays
//! fail-closed and the configuration it reads is never send authority.
//! `app_publish_destinations_list` and its board pair are operator-only —
//! `operator_connection` runs BEFORE any provider/binding work — and a
//! request-forged workspace/grant/caller field can never upgrade auth. The
//! global READ credential's rows are discovery only, not proof they belong
//! to this install's binding/workspace; zero or multiple publication slots
//! refuse before the upstream read rather than selecting an arbitrary one.
//!
//! Guards exercised (mutation record): `Shared::operator_connection` on
//! `rpc_app_binding` for `app_publish_destinations_list` (caller_rule),
//! the `strict_fields` scope-only allowlist (`install_id`,`context_id`),
//! `admit_operator_read` on the app_release read path in
//! `ui/serve.rs`, and the `MediaResolver::list` upstream call — the
//! counting stub proves a refused caller never reaches it (zero GETs)
//! and no state/effect is written.
//!
//! CALLER-PROOF MODEL (CAD-482), stated exactly:
//!
//!   * The seam (`scoped`/`AS_HEADER`/`TOKEN_HEADER`) is used ONLY for
//!     business setup — `setup()` wraps install/bind/publish_set in one
//!     `scoped(Asserted::Operator)` and drops it before any guarded
//!     call. It is never the caller proof under test.
//!
//!   * LITERAL negative caller: the test process itself connects to the
//!     daemon socket with NO seam assertion (`client::rpc` unscoped). `SO_PEERCRED` records its real pid and the daemon
//!     walks its `/proc` ancestry — on this host the process is a
//!     managed worker's tool under an enrolled endpoint, so the literal
//!     derivation is `Caller::Agent(<enrolled runner>)` (a real agent);
//!     on a host with no enrolled runner it derives an unproven caller.
//!     Either way the refusal lands on the real caller-identity path —
//!     resolved identity, never an asserted one, never ambient-dependent
//!     for the refusal itself (the refusal holds for ANY non-operator
//!     derivation).
//!
//!   * POLICY-regression negatives: `scoped(Asserted::Agent/Unproven)`
//!     RPC calls and seam-header HTTP requests exercise the daemon's
//!     `scope_frame` handler and `operator_connection`'s seam branch.
//!     These are asserted identities — they prove the gate keys on the
//!     resolved caller, never that a real caller derived them. They are
//!     labeled as such, never presented as literal proof.
//!
//!   * LITERAL HTTP negative: `http_get_literal` sends NO seam headers —
//!     the request reaches `admit_operator_read`→`board_caller`→
//!     `attribute`→`decide` on the real TCP-peer path and is refused
//!     401/403. This is the board's literal caller check, not asserted.
//!
//!   * POSITIVE control (external): `operator-control.sh` (this dir) is a
//!     complete isolated-fixture harness the operator runs explicitly —
//!     it boots a temp daemon + counting stub, runs the seam-scoped
//!     setup, then makes the guarded call through a real operator-origin
//!     connection. It is NOT `#[ignore]`d cargo code and NOT run in-band
//!     by this suite; the operator executes it when an actual
//!     operator-origin caller is available. See OPERATOR-CONTROL.md.
//!   * Discovery/ambiguity controls use the explicit policy seam only: they
//!     assert the read returns no workspace/send authority and that multiple
//!     publication declarations refuse before an upstream GET. They do not
//!     prove real operator ancestry or a shared AOS workspace.
//!
//! UNEXECUTED (source-only turn): no test/build/fmt run; the confined
//! compile gate and a real operator-origin runner still judge this file.
//!
//! PREREQUISITE — the HTTP counterpart route must exist as an is_read
//! branch admitted by `admit_operator_read`:
//!   GET /api/app-installations/{install}/publish-destinations
//!   GET /api/app-installations/{install}/contexts/{context}/publish-destinations
//! If the implementer lands a different path shape, adjust ONLY the two
//! PATH constants — the refusal contract is what is checked.
#![cfg(feature = "test-seam")]

use cadence_agent::platform::agenticos_external::media_import::MediaResolver;
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use cadence_agent::store::Store;
use cadence_agent::test_seam::{scoped, Asserted, Seam, AS_HEADER, TOKEN_HEADER};
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

const DEST: &str = "275491372109884";
const GRANT: &str = "dpq_destinations_fixture_grant";

type DaemonHandle = (
    Arc<AtomicBool>,
    std::thread::JoinHandle<cadence_agent::Result<()>>,
);

/// One seam-armed daemon on a temp dir with the real `MediaResolver`
/// attached — pointed at a counting stub serving the strict version-1
/// destinations envelope; `reads` is the proof a refused caller never
/// reaches upstream.
struct Fx {
    root: tempfile::TempDir,
    clock: Arc<AtomicI64>,
    daemon: Option<DaemonHandle>,
    store: Store,
    reads: Arc<AtomicU64>,
}

impl Fx {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1143destacc")
            .tempdir()
            .unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let clock = Arc::new(AtomicI64::new(1_800_000_000));
        std::fs::create_dir_all(root.path().join("s")).unwrap();
        let store = Store::open(&root.path().join("s").join("cadence.sqlite3")).unwrap();
        Self {
            root,
            clock,
            daemon: None,
            store,
            reads: Arc::new(AtomicU64::new(0)),
        }
    }
    fn dir(&self) -> std::path::PathBuf {
        self.root.path().join("s")
    }
    fn pm(&self) -> std::path::PathBuf {
        self.root.path().join("pm")
    }
    fn start(&mut self) {
        let (dir, stop) = (self.dir(), Arc::new(AtomicBool::new(false)));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", self.pm().to_str().unwrap());
        let clock = Arc::clone(&self.clock);
        // The counting upstream stub: strict canonical envelope, two rows,
        // and every GET bumps `reads` so a refused caller's absence is
        // measured, not assumed.
        let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = stub.server_addr().to_string();
        let reads = Arc::clone(&self.reads);
        std::thread::spawn(move || {
            let body = json!({"ok": true, "data": {"version": "1", "destinations": [{
                "connectionId": "connA_harbour", "toolkit": "facebook",
                "displayName": "Harbour", "destinationId": DEST,
                "status": "active", "available": true, "publishable": true,
            }, {
                "connectionId": "connA_unbound", "toolkit": "instagram",
                "displayName": "Other read-visible row", "destinationId": "dest-unbound",
                "status": "expired", "available": false, "publishable": false,
            }]}})
            .to_string();
            for request in stub.incoming_requests() {
                reads.fetch_add(1, SeqCst);
                let _ = request.respond(tiny_http::Response::from_string(body.clone()));
            }
        });
        let resolver = MediaResolver::new(
            &format!("http://{addr}"),
            DeviceCredential::new("read-cred".into()),
        )
        .unwrap();
        let mut opts = daemon::ServeOptions {
            provider_env: env,
            stop: Some(Arc::clone(&stop)),
            test_seam: true,
            slots: Some(Default::default()),
            lease: Some(Default::default()),
            auto_stop: Some(daemon::AutoStopSetting::off()),
            agent_gc: Some(Default::default()),
            report_router: Some(0),
            checkup: Some(0),
            operator_clock: Some(Arc::new(move || clock.load(SeqCst))),
            social_media_resolver: Some(Arc::new(resolver)),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3119".into(),
        );
        let handle = std::thread::spawn(move || daemon::serve_with(&dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&self.dir(), "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&self.dir()).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        self.daemon = Some((stop, handle));
    }
    /// A fixture RPC used only inside an explicit seam scope for business
    /// setup. Never the caller proof under test; its enclosing scope is
    /// dropped before the guarded call being checked.
    fn setup_rpc(&self, method: &str, params: Value) -> Value {
        client::rpc(&self.dir(), method, params).unwrap_or_else(|e| panic!("setup {method}: {e}"))
    }
    /// All business setup under one seam scope — install, bind,
    /// publish_set — dropped before any guarded call runs.
    fn setup(&self) -> String {
        scoped(Asserted::Operator, || self.install_scoped())
    }
    /// One install whose manifest declares the publication slot plus a
    /// configured binding carrying publish settings — the real
    /// install → bind → publish_set chain, so the guard's binding
    /// currentness proof has a live target.
    fn install_scoped(&self) -> String {
        let source = self.root.path().join("app-src-dest");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: dest-accept\ntitle: Dest accept\nversion: '0.1.0'\n\
             summary: Destinations-read fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# Dest accept\n",
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).unwrap();
        let installed = self.setup_rpc(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"].as_str().unwrap().to_string();
        let bound = self.setup_rpc(
            "app_binding_create",
            json!({"install_id": install, "slot": "publication",
                "connection_id": self.local_connection(), "request_id": "bind-dest"}),
        );
        let binding = &bound["binding"];
        self.setup_rpc(
            "app_binding_publish_set",
            json!({"install_id": install, "binding_id": binding["id"],
                "expected_revision": binding["revision"],
                "destination_id": DEST, "destination_label": "Harbour",
                "toolkit": "facebook", "timezone": "Asia/Hong_Kong",
                "grant_id": GRANT}),
        );
        install
    }
    fn install_with_two_publication_slots(&self) -> String {
        let source = self.root.path().join("app-src-two-publish-slots");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            r#"---
app: two-publish-slots
title: Two publish slots
version: '0.1.0'
summary: Ambiguous publication-slot fixture.
needs:
  connections: []
  capabilities:
    publication:
      schema: 1
      capability: text.publish
      version: 1
      action: publish
      resource_kind: connection_account
      effect: send
    secondary_publication:
      schema: 1
      capability: text.publish
      version: 1
      action: publish
      resource_kind: connection_account
      effect: send
---

# Two publish slots
"#,
        )
        .unwrap();
        std::fs::write(source.join("workflows/brief.md"), WORKFLOW).unwrap();
        self.setup_rpc(
            "app_workspace_install",
            json!({"source":source.to_str().unwrap()}),
        )["install_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn local_connection(&self) -> String {
        let rows = self.setup_rpc("connection_list", json!({}))["connections"].clone();
        rows.as_array()
            .unwrap()
            .iter()
            .find(|row| row["provider"] == "local" && row["account"] == "local")
            .expect("local builtin connection")["id"]
            .as_str()
            .unwrap()
            .into()
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.daemon.take() {
            stop.store(true, SeqCst);
            let _ = handle.join();
        }
    }
}

#[test]
fn destination_discovery_does_not_claim_read_workspace_or_binding_authority() {
    let mut fx = Fx::new();
    fx.start();
    let install = fx.setup();
    let reads_before = fx.reads.load(SeqCst);
    // POLICY-SEAM positive only: the explicit Operator exercises the real
    // read path, but this is not actual process-ancestry or operator proof.
    let result = scoped(Asserted::Operator, || {
        client::rpc(
            &fx.dir(),
            "app_publish_destinations_list",
            json!({"install_id":install}),
        )
    })
    .unwrap_or_else(|error| panic!("policy-seam discovery refused: {error}"));
    assert_eq!(
        fx.reads.load(SeqCst),
        reads_before + 1,
        "one bounded READ discovery"
    );
    assert_eq!(result["install_id"].as_str(), Some(install.as_str()));
    let mut response_keys: Vec<_> = result
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    response_keys.sort_unstable();
    assert_eq!(response_keys, vec!["destinations", "install_id"]);
    let rows = result["destinations"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "return all READ-visible rows, not binding-filtered rows"
    );
    assert_eq!(rows[1]["destination_id"], "dest-unbound");
    assert_eq!(rows[1]["publishable"], false);
    for row in rows {
        assert!(row.get("workspace").is_none());
        assert!(row.get("grant_id").is_none());
        assert!(row.get("binding_id").is_none());
    }
    assert_eq!(
        fx.intent_and_run_count(&install),
        0,
        "discovery wrote no send/run state"
    );
}

#[test]
fn multiple_publication_slots_refuse_before_a_destinations_read() {
    let mut fx = Fx::new();
    fx.start();
    let install = scoped(Asserted::Operator, || {
        fx.install_with_two_publication_slots()
    });
    let reads_before = fx.reads.load(SeqCst);
    let error = scoped(Asserted::Operator, || {
        client::rpc(
            &fx.dir(),
            "app_publish_destinations_list",
            json!({"install_id":install}),
        )
    })
    .unwrap_err();
    assert!(
        format!("{error}").contains("exactly one publication slot"),
        "multiple publication declarations must refuse, not select one: {error}"
    );
    assert_eq!(
        fx.reads.load(SeqCst),
        reads_before,
        "ambiguous manifest reached the READ credential"
    );
    assert_eq!(fx.intent_and_run_count(&install), 0);
}

const WORKFLOW: &str = r#"---
title: "Dest brief"
goal: "One reviewed brief"
inputs:
  writer: { ask: "writer" }
  reviewer: { ask: "reviewer" }
  subject: { ask: "subject" }
  source: { ask: "facts" }
---

## Write
agent: {{writer}}
size: S
action: local.text.produce

Write one brief about {{subject}} grounded only in {{source}}.

### Acceptance
- [ ] brief exists

## Review
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Review the artifact.

### Acceptance
- [ ] reviewed
"#;

// The board's two read routes (see header prerequisite note).
const PATH_PLAIN: &str = "/api/app-installations/{install}/publish-destinations";
const PATH_CTX: &str = "/api/app-installations/{install}/contexts/{ctx}/publish-destinations";

/// CAD-1143: a non-operator caller on `app_publish_destinations_list` is
/// refused `operator_only` before any provider/binding work — over the
/// daemon RPC AND over the board's HTTP read route — with zero upstream
/// reads and zero state mutation. A request naming a workspace, grant,
/// destination or caller field is refused by `strict_fields` on the same
/// guard path. The seam is scoped to `setup()` only and dropped before
/// any guarded call.
///
/// Literal vs asserted (see header): the negative RPC case runs through
/// `client::rpc` unscoped — this test process connects with NO seam
/// so the daemon derives its caller from `SO_PEERCRED` + `/proc`
/// ancestry. On this host the test process is a managed worker's tool
/// under an enrolled endpoint, so the literal derivation is
/// `Caller::Agent(<enrolled runner>)` — a real agent, refused by the
/// operator gate; on a host with no enrolled runner the same call
/// derives an unproven caller, still refused. Either way the refusal
/// lands on the real caller-identity path, not the seam. The HTTP
/// seam-header requests are POLICY regression only, and the
/// no-seam-header `http_get_literal` request is the literal HTTP bad
/// case. The positive control is the external `operator-control.sh`
/// harness, never an in-band `Asserted::Operator`.
#[test]
fn destinations_read_refuses_non_operator_and_forged_fields_with_zero_upstream_reads() {
    let mut fx = Fx::new();
    fx.start();
    let install = fx.setup();
    let before = fx.reads.load(SeqCst);

    // LITERAL negative caller: this test process connects to the daemon
    // socket with NO seam assertion. `SO_PEERCRED` records our real pid;
    // the daemon walks /proc ancestry and derives a literal non-operator
    // identity — `Caller::Agent(<enrolled runner>)` here (this process
    // is a managed worker's tool), unproven where no runner is enrolled.
    // Refused by the real operator gate, not an asserted identity.
    let err = client::rpc(
        &fx.dir(),
        "app_publish_destinations_list",
        json!({"install_id": install}),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(
        format!("{err}").contains("operator"),
        "literal non-operator caller refused by the wrong guard: {err}"
    );
    // Literal refusal with an explicit context id.
    let err = client::rpc(
        &fx.dir(),
        "app_publish_destinations_list",
        json!({"install_id": install, "context_id": "ctx-any"}),
    )
    .map(|_| ())
    .unwrap_err();
    assert!(
        format!("{err}").contains("operator"),
        "context-scoped literal call refused by the wrong guard: {err}"
    );

    // POLICY-regression negatives (seam-asserted identities — the
    // daemon's scope_frame + operator_connection seam branch): agent and
    // unproven assertions are refused identically, proving the gate keys
    // on the resolved caller. Asserted, not literal proof.
    for who in [
        Asserted::Agent("writer".into()),
        Asserted::Agent("lead".into()),
        Asserted::Unproven,
    ] {
        let err = scoped(who.clone(), || {
            client::rpc(
                &fx.dir(),
                "app_publish_destinations_list",
                json!({"install_id": install}),
            )
        })
        .map(|_| ())
        .unwrap_err();
        assert!(
            format!("{err}").contains("operator"),
            "{who:?} refused by the wrong guard: {err}"
        );
    }

    // A forged privilege field refuses on the same path — strict_fields
    // can never upgrade auth. Checked with an operator-asserted caller
    // (field refusal) and an agent-asserted caller (operator refusal) so
    // no ordering assumption is made.
    for forged in [
        json!({"workspace": "ws-other"}),
        json!({"grant_id": "dpq_forged"}),
        json!({"destination_id": "other-dest"}),
        json!({"caller": "operator"}),
        json!({"context_id": 42}),
    ] {
        let mut params = json!({"install_id": install});
        for (key, value) in forged.as_object().unwrap() {
            params[key] = value.clone();
        }
        let operator_err = scoped(Asserted::Operator, || {
            client::rpc(&fx.dir(), "app_publish_destinations_list", params.clone())
        })
        .map(|_| ())
        .unwrap_err();
        assert!(
            !format!("{operator_err}").contains("destinations"),
            "a forged field reached the resolver: {params}"
        );
        let agent_err = scoped(Asserted::Agent("writer".into()), || {
            client::rpc(&fx.dir(), "app_publish_destinations_list", params)
        })
        .map(|_| ())
        .unwrap_err();
        assert!(
            format!("{agent_err}").contains("operator"),
            "agent-forged field refused by the wrong guard: {agent_err}"
        );
    }

    // HTTP — POLICY regression: seam-header requests assert
    // agent/unproven callers; refused at admit_operator_read →
    // board_caller → attribute (seam branch) → decide. These are
    // asserted identities, NOT literal caller proof.
    for (who, path) in [
        ("agent:writer", PATH_PLAIN.replace("{install}", &install)),
        ("unproven", PATH_PLAIN.replace("{install}", &install)),
        ("agent:lead", PATH_PLAIN.replace("{install}", &install)),
    ] {
        let (status, _body) = http_get(&fx.dir(), who, &path);
        assert_eq!(status, 403, "{who} reached the destinations read: {path}");
    }
    let (status, _) = http_get(
        &fx.dir(),
        "agent:writer",
        &PATH_CTX
            .replace("{install}", &install)
            .replace("{ctx}", "ctx-any"),
    );
    assert_eq!(
        status, 403,
        "context-scoped destinations read reached by an agent"
    );

    // HTTP — LITERAL negative: NO seam headers, NO session cookie. The
    // request reaches attribute on the real TCP-peer path; absent a
    // live operator session or a seam assertion it cannot be admitted to
    // an operator-only read → refused 401/403. This is the board's
    // literal caller check, not asserted.
    let (status, body) = http_get_literal(&fx.dir(), &PATH_PLAIN.replace("{install}", &install));
    assert!(
        status == 401 || status == 403,
        "literal no-identity request reached the destinations read: {status} {body}"
    );
    let (status, _) = http_get_literal(
        &fx.dir(),
        &PATH_CTX
            .replace("{install}", &install)
            .replace("{ctx}", "ctx-any"),
    );
    assert!(
        status == 401 || status == 403,
        "literal no-identity context read reached the resolver"
    );

    // Zero upstream reads and no new install-scoped rows: nothing ran.
    assert_eq!(
        fx.reads.load(SeqCst),
        before,
        "a refused caller reached the upstream destinations read"
    );
    assert_eq!(
        fx.intent_and_run_count(&install),
        0,
        "a refused read wrote state"
    );
}

impl Fx {
    /// The "no write" proof for this read: run rows and publish intents
    /// under this install are the only state a destinations read could
    /// conceivably create; assert both stay zero. Read via the store's
    /// own public list, not an operator RPC — the refused-caller check
    /// must not itself assert operator identity.
    fn intent_and_run_count(&self, install: &str) -> usize {
        let store = &self.store;
        let runs = store.app_run_list_filtered(Some(install), None).unwrap()["runs"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let intents = store.social_publish_list(Some(install), None).unwrap()["intents"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        runs + intents
    }
}

// --------------------------------------------------------------------
// The board's HTTP surface: a seam-attached board on an ephemeral port.
//
// `http_get` (seam headers present) is the POLICY-regression channel —
// the asserted caller in X-Cadence-As + the seam token. It exercises
// admit_operator_read's seam-attribution branch, NOT a literal caller.
//
// `http_get_literal` sends NO seam headers and NO session — the request
// is attributed by the real TCP-peer path only, the board's literal
// caller check for an operator-only read.
// --------------------------------------------------------------------

fn http_get(dir: &Path, who: &str, path: &str) -> (u16, String) {
    let token = Seam::token_at(dir).unwrap();
    http_request(dir, path, Some((who, &token)))
}

fn http_get_literal(dir: &Path, path: &str) -> (u16, String) {
    http_request(dir, path, None)
}

fn http_request(dir: &Path, path: &str, seam: Option<(&str, &str)>) -> (u16, String) {
    let free = |p: &u16| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok();
    let port = (3110..3200).find(free).unwrap();
    let (startup, ready) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let opts = cadence_agent::ui::ServeOpts {
        host: "127.0.0.1".into(),
        port,
        stop: Some(stop.clone()),
        startup: Some(startup),
        test_seam: true,
        ..Default::default()
    };
    let (dir_owned, pm) = (dir.to_path_buf(), dir.join("pm"));
    let board = std::thread::spawn(move || drop(cadence_agent::ui::serve(&dir_owned, &pm, &opts)));
    ready
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let host = format!("cadence-{port}.localhost:{port}");
    let mut req = format!(
        "GET {path} HTTP/1.0\r\nHost: {host}\r\nX-Cadence-Board: 1\r\n\
         Sec-Fetch-Site: same-origin\r\nOrigin: http://{host}\r\n"
    );
    if let Some((who, token)) = seam {
        req.push_str(&format!(
            "{AS_HEADER}: {who}\r\n{TOKEN_HEADER}: {token}\r\n"
        ));
    }
    req.push_str("\r\n");
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    std::io::Write::write_all(&mut s, req.as_bytes()).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut s, &mut text).unwrap();
    stop.store(true, SeqCst);
    let _ = board.join();
    (
        text.split_whitespace().nth(1).unwrap().parse().unwrap(),
        text,
    )
}
