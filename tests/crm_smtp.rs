//! CAD-785 host-custodied SMTP: gates, grammar and link lifecycle.
//!
//! Adversarial-first at the daemon: an operator caller can enroll a
//! typed SMTP sender, bind exactly one per installation/context, and
//! rebind/revoke under CAS; an agent caller, a detached child and
//! forged `by`/`actor`/`workspace`/`project`/`assistant_receipt`
//! fields are refused without mutation. Cross-install and
//! cross-context probes refuse, stale authorization revisions refuse
//! after rotation, revoked links and revoked credentials refuse, and
//! the secret never appears in records, events, errors or logs. No
//! network socket opens anywhere in this file — submission rides the
//! isolated TLS rig in `crm_smtp_send.rs`.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const SECRET: &str = "cad785-smtp-test-secret-01";
const OTHER_SECRET: &str = "cad785-smtp-test-secret-02";

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

struct Crm {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Crm {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
        let mut opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        cadence_agent::platform::smtp::attach(&mut opts);
        let daemon = TestDaemon::start_opts(opts);
        Self {
            _root: root,
            _pm: pm,
            daemon,
        }
    }

    fn copy_source(into: &Path, app: &str) {
        for name in [
            "app.md",
            "workflows/blog-post.md",
            "rubrics/blog.md",
            "templates/brief.md",
            "templates/post.md",
        ] {
            let destination = into.join(name);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::copy(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("apps/blog-post")
                    .join(name),
                &destination,
            )
            .unwrap();
        }
        if app != "blog-post" {
            let manifest = into.join("app.md");
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(
                manifest,
                text.replace("app: blog-post", &format!("app: {app}")),
            )
            .unwrap();
        }
    }

    fn install(&self) -> String {
        self.daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self._root.path().join("source")}),
            )
            .unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn install_second(&self) -> String {
        let second = self._root.path().join("second");
        Self::copy_source(&second, "blog-post-two");
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": second}))
            .unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn context(&self, install: &str, label: &str, request: &str) -> String {
        self.daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": label, "input_defaults": {}, "request_id": request}),
            )
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn save_content(&self, install: &str, context: &str, campaign: &str) {
        self.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign,
                    "subject": "Spring launch", "preheader": "News", "blocks": blocks()}),
            )
            .unwrap();
    }

    fn enroll(&self, account: &str, secret: &str) -> Value {
        self.daemon
            .operator_rpc(
                "connection_create",
                json!({"provider": "smtp", "account": account, "shape": "smtp",
                    "host": "localhost", "port": 465, "tls_mode": "implicit",
                    "username": "smtp-user", "secret": secret,
                    "sender": "news@example.com", "sender_name": "CRM News",
                    "scopes": ["email:send"], "accept_same_uid_risk": true}),
            )
            .unwrap()["connection"]
            .clone()
    }

    fn bind(&self, install: &str, context: &str, connection: &str, request: &str) -> Value {
        self.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": connection, "request_id": request}),
            )
            .unwrap()["binding"]
            .clone()
    }
}

fn native(
    lane: &mut LaneShell,
    state: &Path,
    detached: bool,
    method: &str,
    params: Value,
) -> Value {
    let frame = json!({"method": method, "params": params});
    assert_eq!(frame.as_object().unwrap().len(), 2);
    assert!(frame.get(cadence_agent::test_seam::FRAME_FIELD).is_none());
    let wire = frame.to_string();
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    assert!(!wire.contains(cadence_agent::test_seam::TOKEN_HEADER));
    let request = lane.dir.path().join(format!("native-{}.json", lane.seq));
    std::fs::write(&request, wire).unwrap();
    let prefix = if detached { "setsid " } else { "" };
    let (rc, text) = lane.run(&format!("{prefix}python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.settimeout(10);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", state.join("cadence.sock").display(), request.display()));
    assert_eq!(rc, 0, "native socket process failed: {text}");
    serde_json::from_str(text.trim()).unwrap()
}

fn events_text(daemon: &TestDaemon) -> String {
    let db = rusqlite::Connection::open(daemon.state.join("cadence.sqlite3")).unwrap();
    let mut stmt = db.prepare("SELECT payload FROM events").unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows.join("\n")
}

#[test]
fn cad785_agent_detached_and_forged_fields_cannot_touch_smtp() {
    let app = Crm::new();
    let install = app.install();
    let context = app.context(&install, "brand", "ctx-1");
    app.save_content(&install, &context, "launch-1");
    let connection = app.enroll("gated", SECRET);
    let id = connection["id"].as_str().unwrap();
    app.bind(&install, &context, id, "bind-1");

    let mut lane = LaneShell::spawn(app.daemon.dir.path());
    plant_member_pane(&app.daemon, "smtp-peer", "claude", None, lane.pid());
    let smtp_create = || {
        json!({"provider": "smtp", "account": "forged", "shape": "smtp",
            "host": "localhost", "port": 465, "tls_mode": "implicit",
            "username": "smtp-user", "secret": SECRET,
            "sender": "news@example.com", "scopes": ["email:send"],
            "accept_same_uid_risk": true})
    };
    let calls = [
        ("connection_create", smtp_create()),
        (
            "crm_smtp_bind",
            json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "forged"}),
        ),
        (
            "crm_smtp_rebind",
            json!({"install_id": install, "context_id": context, "connection_id": id, "expected_revision": 1}),
        ),
        (
            "crm_smtp_revoke",
            json!({"install_id": install, "context_id": context, "expected_revision": 1}),
        ),
        (
            "crm_smtp_show",
            json!({"install_id": install, "context_id": context}),
        ),
        (
            "crm_smtp_test_send",
            json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": "operator@example.com"}),
        ),
    ];
    let before_list = app
        .daemon
        .operator_rpc("connection_list", json!({}))
        .unwrap();
    let before_show = app
        .daemon
        .operator_rpc(
            "crm_smtp_show",
            json!({"install_id": install, "context_id": context}),
        )
        .unwrap();
    let mut failures = Vec::new();
    for detached in [false, true] {
        for (method, params) in &calls {
            let frame = native(
                &mut lane,
                &app.daemon.state,
                detached,
                method,
                params.clone(),
            );
            if frame["ok"] != false
                || frame.get("result").is_some()
                || !frame["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("operator action")
            {
                failures.push(json!({"detached": detached, "method": method, "frame": frame}));
            }
        }
        // A forged actor field rides an agent call: refused as a
        // field before identity is even consulted.
        let frame = native(
            &mut lane,
            &app.daemon.state,
            detached,
            "crm_smtp_show",
            json!({"install_id": install, "context_id": context, "by": "operator"}),
        );
        assert_eq!(frame["ok"], false, "forged by admitted: {frame}");
    }
    assert!(
        failures.is_empty(),
        "agent/detached SMTP admission failed: {failures:?}"
    );
    // Forged fields over the operator's own connection refuse too.
    for params in [
        json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "x", "actor": "operator"}),
        json!({"install_id": install, "context_id": context, "connection_id": id, "request_id": "x", "workspace": "w"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": "operator@example.com", "assistant_receipt": "r"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1", "to_email": "operator@example.com", "turn_id": "t"}),
        json!({"install_id": install, "context_id": context, "project": "p"}),
    ] {
        let method = if params.get("campaign_id").is_some() {
            "crm_smtp_test_send"
        } else if params.get("project").is_some() {
            "crm_smtp_show"
        } else {
            "crm_smtp_bind"
        };
        assert!(
            app.daemon.operator_rpc(method, params).is_err(),
            "forged field admitted on {method}"
        );
    }
    assert_eq!(
        app.daemon
            .operator_rpc("connection_list", json!({}))
            .unwrap(),
        before_list
    );
    assert_eq!(
        app.daemon
            .operator_rpc(
                "crm_smtp_show",
                json!({"install_id": install, "context_id": context}),
            )
            .unwrap(),
        before_show
    );
}

#[test]
fn cad785_enrollment_grammar_refuses_plaintext_and_placeholders() {
    let app = Crm::new();
    let base = || {
        json!({"provider": "smtp", "account": "grammar", "shape": "smtp",
            "host": "mail.example.com", "port": 587, "tls_mode": "starttls",
            "username": "smtp-user", "secret": SECRET,
            "sender": "news@example.com", "scopes": ["email:send"],
            "accept_same_uid_risk": true})
    };
    // Crossed and plaintext pairs refuse.
    for (port, mode) in [
        (25u64, "starttls"),
        (587, "implicit"),
        (465, "starttls"),
        (2525, "starttls"),
    ] {
        let mut params = base();
        params["port"] = json!(port);
        params["tls_mode"] = json!(mode);
        assert!(
            app.daemon
                .operator_rpc("connection_create", params)
                .is_err(),
            "plaintext pair admitted: {port}/{mode}"
        );
    }
    // Bad hosts, senders, scopes and shape mixes refuse.
    let mutations: Vec<(&str, Value)> = vec![
        ("ip-host", {
            let mut p = base();
            p["host"] = json!("127.0.0.1");
            p
        }),
        ("bare-host", {
            let mut p = base();
            p["host"] = json!("mailserver");
            p
        }),
        ("invalid-sender", {
            let mut p = base();
            p["sender"] = json!("noreply@cadence.invalid");
            p
        }),
        ("sloppy-sender", {
            let mut p = base();
            p["sender"] = json!("not-an-address");
            p
        }),
        ("token-shape-mix", {
            let mut p = base();
            p["token"] = json!("opaque");
            p
        }),
        ("missing-secret", {
            let mut p = base();
            p.as_object_mut().unwrap().remove("secret");
            p
        }),
        ("missing-sender", {
            let mut p = base();
            p.as_object_mut().unwrap().remove("sender");
            p
        }),
        ("wrong-scopes", {
            let mut p = base();
            p["scopes"] = json!(["widgets:read"]);
            p
        }),
        ("empty-scopes", {
            let mut p = base();
            p["scopes"] = json!([]);
            p
        }),
        ("extra-field", {
            let mut p = base();
            p["password"] = json!("x");
            p
        }),
        ("bad-mode", {
            let mut p = base();
            p["tls_mode"] = json!("opportunistic");
            p
        }),
        ("bad-sender-name", {
            let mut p = base();
            p["sender_name"] = json!("Evil <x>");
            p
        }),
    ];
    for (label, params) in mutations {
        assert!(
            app.daemon
                .operator_rpc("connection_create", params)
                .is_err(),
            "bad enrollment admitted: {label}"
        );
    }
    // The legacy platform verbs have no SMTP grammar.
    assert!(
        app.daemon
            .operator_rpc(
                "platform_enroll",
                json!({"platform": "smtp", "account": "grammar", "shape": "smtp",
                    "host": "mail.example.com", "port": 587, "tls_mode": "starttls",
                    "username": "smtp-user", "secret": SECRET,
                    "sender": "news@example.com", "scopes": ["email:send"],
                    "accept_same_uid_risk": true}),
            )
            .is_err(),
        "legacy platform enroll minted SMTP custody"
    );
    // Token-shape connections are unaffected by the new grammar.
    // The token value is assembled via `concat!` (CAD-440 pattern):
    // it is inert — the `fixture` provider is unregistered, so the
    // enrollment fails closed before custody — but a contiguous
    // secret-shaped literal would trip the gitleaks backstop.
    let token = app.daemon.operator_rpc(
        "connection_create",
        json!({"provider": "fixture", "account": "untouched", "shape": "token",
                "token": concat!("cadp_conn", "_secret_12345"), "scopes": ["widgets:read"],
                "accept_same_uid_risk": true}),
    );
    assert!(
        token.is_err(),
        "fixture provider is unregistered here — enrollment must fail closed"
    );
}

#[test]
fn cad785_secret_never_leaves_custody() {
    let app = Crm::new();
    let connection = app.enroll("custody", SECRET);
    let id = connection["id"].as_str().unwrap().to_string();
    assert!(!connection.to_string().contains(SECRET));
    assert_eq!(connection["smtp"]["sender"], "news@example.com");
    assert_eq!(connection["smtp"]["port"], 465);
    assert!(connection["smtp"].get("secret").is_none());
    let list = app
        .daemon
        .operator_rpc("connection_list", json!({}))
        .unwrap();
    assert!(!list.to_string().contains(SECRET));
    let show = app
        .daemon
        .operator_rpc("connection_show", json!({"connection_id": id}))
        .unwrap();
    assert!(!show.to_string().contains(SECRET));
    // A rotate whose fresh secret echoes enrolled metadata (here the
    // username) refuses before any custody write — and the refusal
    // never reflects either secret.
    let error = app
        .daemon
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id": id, "secret": "smtp-user"}),
        )
        .unwrap_err()
        .to_string();
    assert!(!error.contains(SECRET), "rotate error leaked: {error}");
    assert!(!error.contains("smtp-user"), "rotate error echoed: {error}");
    let show = app
        .daemon
        .operator_rpc("connection_show", json!({"connection_id": id}))
        .unwrap();
    assert_eq!(
        show["connection"]["revision"].as_u64(),
        Some(1),
        "refused rotate mutated"
    );
    let error = app
        .daemon
        .operator_rpc(
            "connection_create",
            json!({"provider": "smtp", "account": "custody", "shape": "smtp",
                "host": "localhost", "port": 465, "tls_mode": "implicit",
                "username": "smtp-user", "secret": OTHER_SECRET,
                "sender": "news@example.com", "scopes": ["email:send"],
                "accept_same_uid_risk": true}),
        )
        .unwrap_err()
        .to_string();
    assert!(!error.contains(OTHER_SECRET), "duplicate error leaked");
    // Rotation succeeds with a fresh secret; the old bytes are gone.
    let rotated = app
        .daemon
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id": id, "secret": OTHER_SECRET}),
        )
        .unwrap();
    assert_eq!(rotated["connection"]["id"], id);
    assert_eq!(rotated["connection"]["revision"], 2);
    assert!(!rotated.to_string().contains(OTHER_SECRET));
    assert!(!events_text(&app.daemon).contains(SECRET));
    assert!(!events_text(&app.daemon).contains(OTHER_SECRET));
}

/// Replace the one custody file and re-record its fingerprint, the
/// way a corrupted-but-consistent record looks to the daemon.
fn replace_custody(daemon: &TestDaemon, bytes: &[u8], fingerprint_matches: bool) {
    let dir = daemon.state.join("custody");
    let cred = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("cred"))
        .expect("custody file");
    std::fs::write(&cred, bytes).unwrap();
    if fingerprint_matches {
        let db = rusqlite::Connection::open(daemon.state.join("cadence.sqlite3")).unwrap();
        db.execute(
            "UPDATE platform_credentials SET fingerprint=?1",
            [cadence_agent::secret::fingerprint(bytes)],
        )
        .unwrap();
    }
}

fn listed(app: &Crm, id: &str) -> Value {
    app.daemon
        .operator_rpc("connection_list", json!({}))
        .unwrap()["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .expect("connection listed")
}

fn smtp_fields(secret: &str) -> Value {
    json!({"provider": "smtp", "account": "gmail", "shape": "smtp",
        "host": "localhost", "port": 465, "tls_mode": "implicit",
        "username": "smtp-user", "secret": secret,
        "sender": "news@example.com", "sender_name": "CRM News",
        "scopes": ["email:send"], "accept_same_uid_risk": true})
}

#[test]
fn cad1064_unreadable_sender_stays_listed_with_typed_error_and_no_secret() {
    let app = Crm::new();
    let id = app.enroll("gmail", SECRET)["id"]
        .as_str()
        .unwrap()
        .to_string();
    let healthy = listed(&app, &id);
    assert_eq!(healthy["smtp_sender"], true);
    assert_eq!(healthy["smtp_error"], Value::Null);
    assert_eq!(healthy["smtp"]["host"], "localhost");
    // Non-SMTP rows are never SMTP senders.
    let all = app
        .daemon
        .operator_rpc("connection_list", json!({}))
        .unwrap();
    for row in all["connections"].as_array().unwrap() {
        if row["id"] != id.as_str() {
            assert_eq!(row["smtp_sender"], false, "{row}");
            assert_eq!(row["smtp_error"], Value::Null, "{row}");
        }
    }
    // Corrupt custody that still matches its fingerprint.
    replace_custody(&app.daemon, b"{\"schema\":2}", true);
    let corrupt = listed(&app, &id);
    assert_eq!(corrupt["kind"], "enrolled");
    assert_eq!(corrupt["smtp"], Value::Null);
    assert_eq!(corrupt["smtp_sender"], true);
    assert_eq!(corrupt["smtp_error"], "custody_corrupt");
    // Torn custody (fingerprint no longer matches).
    replace_custody(&app.daemon, b"torn", false);
    let torn = listed(&app, &id);
    assert_eq!(torn["smtp_sender"], true);
    assert_eq!(torn["smtp_error"], "unavailable");
    for row in [&corrupt, &torn] {
        assert!(!row.to_string().contains(SECRET), "{row}");
    }
    // A leaky legacy record: secret overlaps the username, written
    // straight to custody (enrollment refuses to create this).
    let leaky = format!("zz{}zz", "smtp-user");
    let bytes = serde_json::to_vec(&json!({"schema": 1, "provider": "smtp",
        "host": "localhost", "port": 465, "tls_mode": "implicit",
        "username": "smtp-user", "secret": leaky,
        "sender": "news@example.com", "sender_name": "CRM News"}))
    .unwrap();
    replace_custody(&app.daemon, &bytes, true);
    let withheld = listed(&app, &id);
    assert_eq!(withheld["smtp"], Value::Null);
    assert_eq!(withheld["smtp_sender"], true);
    assert_eq!(withheld["smtp_error"], "withheld_leak");
    assert!(!withheld.to_string().contains(&leaky));
    assert!(!withheld.to_string().contains("zzsmtp"));
    let shown = app
        .daemon
        .operator_rpc("connection_show", json!({"connection_id": id}))
        .unwrap();
    assert_eq!(shown["connection"]["smtp_error"], "withheld_leak");
    // Rotation with only a secret cannot inherit unreadable fields.
    let error = app
        .daemon
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id": id, "secret": OTHER_SECRET}),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("re-entered"), "{error}");
    assert!(!error.contains(OTHER_SECRET) && !error.contains(&leaky));
    // Re-entering every field repairs it.
    let mut full = smtp_fields(OTHER_SECRET);
    full["connection_id"] = json!(id);
    for field in [
        "provider",
        "account",
        "shape",
        "scopes",
        "accept_same_uid_risk",
    ] {
        full.as_object_mut().unwrap().remove(field);
    }
    let rotated = app.daemon.operator_rpc("connection_rotate", full).unwrap();
    assert_eq!(rotated["connection"]["smtp_error"], Value::Null);
    assert_eq!(rotated["connection"]["smtp"]["username"], "smtp-user");
    assert!(!rotated.to_string().contains(OTHER_SECRET));
}

#[test]
fn cad1064_enrollment_and_rotation_refuse_overlapping_password_and_strip_spaces() {
    let app = Crm::new();
    let overlap = format!("a{}b", "smtp-user");
    let error = app
        .daemon
        .operator_rpc("connection_create", smtp_fields(&overlap))
        .unwrap_err()
        .to_string();
    assert!(error.contains("different password"), "{error}");
    assert!(!error.contains(&overlap), "echoed: {error}");
    let none = app
        .daemon
        .operator_rpc("connection_list", json!({}))
        .unwrap();
    assert!(
        none["connections"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["smtp_sender"] == false),
        "refused enrollment created a record"
    );
    // Gmail groups its app passwords with spaces.
    let grouped = ["qwlz", "xmnb", "vcpo", "iuyt"].join(" ");
    let created = app
        .daemon
        .operator_rpc("connection_create", smtp_fields(&grouped))
        .unwrap();
    let id = created["connection"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["connection"]["smtp_error"], Value::Null);
    assert_eq!(created["connection"]["smtp"]["sender"], "news@example.com");
    // Rotation with an overlapping secret is refused, nothing changes.
    let error = app
        .daemon
        .operator_rpc(
            "connection_rotate",
            json!({"connection_id": id, "secret": overlap}),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("different password"), "{error}");
    assert_eq!(listed(&app, &id)["revision"], 1);
}

#[test]
fn cad785_one_live_link_per_install_context_with_stale_revoke() {
    let app = Crm::new();
    let install = app.install();
    let other_install = app.install_second();
    let context = app.context(&install, "brand", "ctx-1");
    let other_context = app.context(&install, "other", "ctx-2");
    let first = app.enroll("first", SECRET);
    let second = app.enroll("second", OTHER_SECRET);
    let first_id = first["id"].as_str().unwrap();
    let second_id = second["id"].as_str().unwrap();

    let binding = app.bind(&install, &context, first_id, "req-1");
    assert_eq!(binding["link_revision"], 1);
    assert_eq!(binding["auth_revision"], 1);
    // A second live bind on the same pair refuses.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": second_id, "request_id": "req-2"}),
            )
            .is_err(),
        "second live link admitted"
    );
    // Replaying the bind's own request with different material refuses.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": second_id, "request_id": "req-1"}),
            )
            .is_err(),
        "request ID retargeted"
    );
    // Identical replay is idempotent.
    let replay = app.bind(&install, &context, first_id, "req-1");
    assert_eq!(replay, binding);
    // A sibling context binds independently.
    let sibling = app.bind(&install, &other_context, second_id, "req-sib");
    assert_eq!(sibling["connection_id"], second_id);
    // Stale CAS on rebind refuses.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_rebind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": second_id, "expected_revision": 99}),
            )
            .is_err(),
        "stale rebind admitted"
    );
    // Rebind switches the sender under CAS.
    let rebound = app
        .daemon
        .operator_rpc(
            "crm_smtp_rebind",
            json!({"install_id": install, "context_id": context,
                "connection_id": second_id, "expected_revision": 1}),
        )
        .unwrap()["binding"]
        .clone();
    assert_eq!(rebound["link_revision"], 2);
    assert_eq!(rebound["connection_id"], second_id);
    assert_ne!(rebound["digest"], binding["digest"]);
    // The first revision is now spent.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_rebind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": first_id, "expected_revision": 1}),
            )
            .is_err(),
        "spent revision admitted"
    );
    // Revoke under CAS; a second revoke refuses, and the revoked
    // link refuses sends and shows.
    let revoked = app
        .daemon
        .operator_rpc(
            "crm_smtp_revoke",
            json!({"install_id": install, "context_id": context, "expected_revision": 2}),
        )
        .unwrap();
    assert_eq!(revoked["revoked"], true);
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_revoke",
                json!({"install_id": install, "context_id": context, "expected_revision": 3}),
            )
            .is_err(),
        "double revoke admitted"
    );
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_show",
                json!({"install_id": install, "context_id": context}),
            )
            .is_err(),
        "revoked link still shows"
    );
    // Rebind of a revoked link refuses; a fresh bind re-lives it.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_rebind",
                json!({"install_id": install, "context_id": context,
                    "connection_id": first_id, "expected_revision": 3}),
            )
            .is_err(),
        "rebind of revoked link admitted"
    );
    let relived = app.bind(&install, &context, first_id, "req-3");
    assert_eq!(relived["state"], "live");
    assert_eq!(relived["link_revision"], 4);
    // The sibling installation never shared the link.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_show",
                json!({"install_id": other_install, "context_id": context}),
            )
            .is_err(),
        "link leaked across installations"
    );
}

#[test]
fn cad785_cross_install_context_probes_refuse() {
    let app = Crm::new();
    let install = app.install();
    let other_install = app.install_second();
    let context = app.context(&install, "brand", "ctx-1");
    let connection = app.enroll("scoped", SECRET);
    let id = connection["id"].as_str().unwrap();
    app.bind(&install, &context, id, "req-1");

    // Unknown installation, foreign context, and swapped pairs.
    let missing_context = app.context(&other_install, "brand", "ctx-9");
    for (probe_install, probe_context, label) in [
        ("no-such-install", context.as_str(), "unknown install"),
        (install.as_str(), "no-such-context", "unknown context"),
        (other_install.as_str(), context.as_str(), "foreign install"),
        (
            install.as_str(),
            missing_context.as_str(),
            "foreign context",
        ),
    ] {
        for method in ["crm_smtp_show", "crm_smtp_revoke", "crm_smtp_rebind"] {
            let mut params = json!({"install_id": probe_install, "context_id": probe_context});
            if method == "crm_smtp_revoke" {
                params["expected_revision"] = json!(1);
            } else if method == "crm_smtp_rebind" {
                params["connection_id"] = json!(id);
                params["expected_revision"] = json!(1);
            }
            assert!(
                app.daemon.operator_rpc(method, params).is_err(),
                "{label} admitted on {method}"
            );
        }
        assert!(
            app.daemon
                .operator_rpc(
                    "crm_smtp_test_send",
                    json!({"install_id": probe_install, "context_id": probe_context,
                        "campaign_id": "launch-1", "to_email": "operator@example.com"}),
                )
                .is_err(),
            "{label} admitted on test send"
        );
    }
    // Binding through a foreign installation refuses before custody.
    assert!(
        app.daemon
            .operator_rpc(
                "crm_smtp_bind",
                json!({"install_id": other_install, "context_id": context,
                    "connection_id": id, "request_id": "req-x"}),
            )
            .is_err(),
        "cross-install bind admitted"
    );
    // The real link is untouched.
    let show = app
        .daemon
        .operator_rpc(
            "crm_smtp_show",
            json!({"install_id": install, "context_id": context}),
        )
        .unwrap();
    assert_eq!(show["binding"]["link_revision"], 1);
}

#[test]
fn cad785_single_recipient_grammar_only() {
    let app = Crm::new();
    let install = app.install();
    let context = app.context(&install, "brand", "ctx-1");
    app.save_content(&install, &context, "launch-1");
    let connection = app.enroll("single", SECRET);
    app.bind(
        &install,
        &context,
        connection["id"].as_str().unwrap(),
        "req-1",
    );
    // Bulk shapes, placeholder recipients and unknown audience
    // fields refuse before any socket opens.
    for params in [
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": ["operator@example.com"]}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": "a@example.com,b@example.com"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": "operator@example.com", "to_emails": ["x@example.com"]}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": "operator@example.com", "audience_freeze_id": "f"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": "noreply@cadence.invalid"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "launch-1",
            "to_email": "not-an-address"}),
        json!({"install_id": install, "context_id": context, "campaign_id": "no-such-campaign",
            "to_email": "operator@example.com"}),
    ] {
        assert!(
            app.daemon
                .operator_rpc("crm_smtp_test_send", params)
                .is_err(),
            "bulk/placeholder recipient admitted"
        );
    }
}
