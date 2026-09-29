//! CAD-779 CRM MVP: customer search/pagination, extended profile and CSV
//! preview/import on the per-installation record store.
//!
//! Adversarial-first: these tests name each new guard before the
//! implementation exists. Every test below fails while the new host
//! actions (`app_record_csv_preview`, `app_record_csv_import`), the
//! extended list parameters and the extended profile shape are
//! unknown — the RED receipt — and passes once the guards land.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":["vip"],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;

const CSV_MIXED: &str = "record_id,display_name,email,tags,consent_email,expected_revision\n\
customer-9,Chidi Anagonye,chidi@example.com,newcomer,granted,\n\
customer-1,Amina Diallo,amina@example.com,vip,granted,\n\
customer-2,Boris Feld Jr,boris@example.com,,denied,\n\
customer-3,Chidi Anagonye Jr,chidi3@example.com,,granted,1\n\
customer-bad,Bad Row,not-an-email,,granted,\n";
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Chidi Anagonye","email":"chidi3@example.com","tags":[],"consent":{"email":"granted"}}"#;

struct Records {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Records {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        Self::copy_source(&root.path().join("source"), "blog-post");
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Self {
            _root: root,
            _pm: pm,
            daemon,
        }
    }

    fn copy_source(into: &std::path::Path, app: &str) {
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

    fn install(&self) -> Value {
        self.daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": self._root.path().join("source")}),
            )
            .unwrap()
    }

    fn install_second(&self) -> Value {
        let second = self._root.path().join("second");
        Self::copy_source(&second, "blog-post-two");
        self.daemon
            .operator_rpc("app_workspace_install", json!({"source": second}))
            .unwrap()
    }

    fn context(&self, install: &str, label: &str, request: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": label, "input_defaults": {}, "request_id": request}),
            )
            .unwrap()["context"]
            .clone()
    }

    fn profile(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn create(&self, install: &str, context: &str, record: &str, profile: Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context, "record_id": record, "profile": profile}),
            )
            .unwrap()
    }

    fn show(&self, install: &str, context: &str, record: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": install, "context_id": context, "record_id": record}),
            )
            .unwrap()
    }

    fn preview(&self, install: &str, context: &str, csv: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context, "csv_text": csv}),
            )
            .unwrap()
    }

    fn import(&self, install: &str, context: &str, csv: &str, token: &str, request: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context, "csv_text": csv, "preview_token": token, "request_id": request}),
            )
            .unwrap()
    }
}

fn row(preview: &Value, number: i64) -> &Value {
    preview["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["row"] == number)
        .unwrap_or_else(|| panic!("preview missing row {number}: {preview}"))
}

#[test]
fn cad779_csv_preview_reports_create_update_skip_and_row_errors() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    w.create(
        install,
        context_id,
        "customer-2",
        Records::profile(PROFILE_B),
    );
    w.create(
        install,
        context_id,
        "customer-3",
        Records::profile(PROFILE_C),
    );

    let preview = w.preview(install, context_id, CSV_MIXED);
    let token = preview["preview_token"].as_str().unwrap().to_string();
    assert!(
        token.starts_with("sha256:"),
        "preview token unbound: {preview}"
    );
    assert_eq!(
        preview["summary"],
        json!({"create": 1, "update": 1, "skip": 1, "needs_revision": 1, "error": 1})
    );
    // A fresh row plans a create with its parsed profile attached.
    let fresh = row(&preview, 1);
    assert_eq!(fresh["decision"], "create");
    assert_eq!(fresh["record_id"], "customer-9");
    assert_eq!(fresh["profile"]["display_name"], "Chidi Anagonye");
    // An identical row is a skip, not a duplicate write.
    let same = row(&preview, 2);
    assert_eq!(same["decision"], "skip");
    assert_eq!(same["record_id"], "customer-1");
    // A changed row without an expected revision waits for explicit CAS.
    let changed = row(&preview, 3);
    assert_eq!(changed["record_id"], "customer-2");
    assert_eq!(changed["decision"], "needs_revision");
    // A changed row with the current revision plans an update.
    let update = row(&preview, 4);
    assert_eq!(update["record_id"], "customer-3");
    assert_eq!(update["decision"], "update");
    assert_eq!(update["expected_revision"], 1);
    // The malformed row errors with a code, never the offending value.
    let bad = row(&preview, 5);
    assert_eq!(bad["decision"], "error");
    let errors = bad["errors"].as_array().unwrap();
    assert!(!errors.is_empty());
    assert!(bad.to_string().contains("invalid email"));
    assert!(!preview.to_string().contains("not-an-email"));
    // Preview never mutates: only the seeded records exist.
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 3);
}

#[test]
fn cad779_csv_import_applies_plan_with_idempotent_retry_and_stale_proof() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );

    let csv = "record_id,display_name,email,tags,consent_email,expected_revision\n\
customer-9,Chidi Anagonye,chidi@example.com,newcomer,granted,\n\
customer-1,Amina Diallo,amina@example.com,vip,granted,\n\
customer-bad,Bad Row,not-an-email,,granted,\n";
    let preview = w.preview(install, context_id, csv);
    let token = preview["preview_token"].as_str().unwrap().to_string();

    let imported = w.import(install, context_id, csv, &token, "req-1");
    assert_eq!(imported["request_id"], "req-1");
    assert_eq!(imported["replayed"], false);
    assert_eq!(
        imported["summary"],
        json!({"applied": 1, "skipped": 2, "failed": 0})
    );
    assert_eq!(
        w.show(install, context_id, "customer-9")["record"]["revision"],
        1
    );
    // The malformed row never created a record.
    assert!(w
        .daemon
        .operator_rpc(
            "app_record_show",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-bad"}),
        )
        .is_err());

    // An identical retry replays the stored receipt without re-applying.
    let replayed = w.import(install, context_id, csv, &token, "req-1");
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["summary"], imported["summary"]);
    assert_eq!(replayed["rows"], imported["rows"]);

    // A tampered CSV behind a valid token is a stale plan: refused.
    let tampered = csv.replace("Chidi Anagonye", "Chidi Other");
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context_id, "csv_text": tampered, "preview_token": token, "request_id": "req-2"}),
            )
            .is_err(),
        "stale preview token accepted a changed CSV"
    );
    // A reused request id behind different bytes is refused, never merged.
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context_id, "csv_text": tampered, "preview_token": "sha256:tampered", "request_id": "req-1"}),
            )
            .is_err(),
        "request id reuse accepted different bytes"
    );
    assert_eq!(
        w.show(install, context_id, "customer-9")["record"]["revision"],
        1
    );

    // A stale expected revision fails its row without touching the record.
    let before = w.show(install, context_id, "customer-1");
    let stale_csv = "record_id,display_name,email,tags,consent_email,expected_revision\n\
customer-1,Amina Changed,amina@example.com,vip,granted,7\n";
    let stale_preview = w.preview(install, context_id, stale_csv);
    let stale_token = stale_preview["preview_token"].as_str().unwrap().to_string();
    let stale_result = w.import(install, context_id, stale_csv, &stale_token, "req-stale");
    assert_eq!(stale_result["summary"]["failed"], 1);
    assert_eq!(
        w.show(install, context_id, "customer-1")["record"],
        before["record"]
    );
}

#[test]
fn cad779_csv_consent_is_never_inferred_and_duplicates_refuse() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );

    // Absent consent columns arrive as unknown — never granted.
    let csv = "record_id,display_name,email\ncustomer-9,Chidi Anagonye,chidi@example.com\n";
    let token = w.preview(install, context_id, csv)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();
    w.import(install, context_id, csv, &token, "req-consent");
    let shown = w.show(install, context_id, "customer-9");
    assert_eq!(shown["record"]["profile"]["consent"]["email"], "unknown");

    // An unknown consent value in the CSV is a row error, not a grant.
    let maybe_csv = "record_id,display_name,email,consent_email\ncustomer-8,Maybe Man,maybe@example.com,maybe\n";
    let maybe_preview = w.preview(install, context_id, maybe_csv);
    assert_eq!(row(&maybe_preview, 1)["decision"], "error");

    // A new id behind an existing email is a duplicate candidate: refused.
    let dup_csv = "record_id,display_name,email\ncustomer-10,Amina Copy,amina@example.com\n";
    let dup_preview = w.preview(install, context_id, dup_csv);
    let dup_row = row(&dup_preview, 1);
    assert_eq!(dup_row["decision"], "error");
    assert_eq!(dup_row["duplicate_of"], "customer-1");
    assert!(dup_row.to_string().contains("duplicate"));

    // A repeated record id inside one CSV errors both rows.
    let self_dup = "record_id,display_name,email\ncustomer-11,First One,first@example.com\ncustomer-11,Second One,second@example.com\n";
    let self_preview = w.preview(install, context_id, self_dup);
    assert_eq!(row(&self_preview, 1)["decision"], "error");
    assert_eq!(row(&self_preview, 2)["decision"], "error");

    // Explicit decisions cannot rescue error rows: the import refuses.
    let dup_token = dup_preview["preview_token"].as_str().unwrap().to_string();
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context_id, "csv_text": dup_csv, "preview_token": dup_token, "request_id": "req-force", "decisions": [{"row": 1, "action": "create"}]}),
            )
            .is_err(),
        "explicit decision rescued a duplicate row"
    );
    // Nothing above created rows: exact inventory asserted.
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    let mut ids: Vec<String> = listed["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["customer-1".to_string(), "customer-9".to_string()]
    );
}

#[test]
fn cad779_csv_agent_forged_and_detached_callers_are_refused() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    let csv = "record_id,display_name,email\ncustomer-9,Chidi Anagonye,chidi@example.com\n";
    // Valid operator control first: both actions answer.
    let token = w.preview(install, context_id, csv)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(token.starts_with("sha256:"));

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "csv-worker", "claude", None, lane.pid());
    for (method, params) in [
        (
            "app_record_csv_preview",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv}),
        ),
        (
            "app_record_csv_import",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "preview_token": token, "request_id": "req-agent"}),
        ),
    ] {
        let frame = lane.rpc(&w.daemon.state, method, params);
        assert_eq!(frame["ok"], false, "agent reached {method}");
        assert!(
            frame.to_string().contains("operator"),
            "agent refusal missed caller authority for {method}: {frame}"
        );
    }
    // Forged identity and discovery-link fields refuse before any plan.
    for (method, params) in [
        (
            "app_record_csv_preview",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "by": "operator"}),
        ),
        (
            "app_record_csv_preview",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "actor": "operator"}),
        ),
        (
            "app_record_csv_preview",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "project": "client"}),
        ),
        (
            "app_record_csv_import",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "preview_token": token, "request_id": "req-forged", "workspace": "default"}),
        ),
        (
            "app_record_csv_import",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "preview_token": token, "request_id": "req-forged", "project_link": "client"}),
        ),
    ] {
        let frame = lane.rpc(&w.daemon.state, method, params);
        assert_eq!(
            frame["ok"], false,
            "forged fields reached {method}: {frame}"
        );
    }
    // A detached child of the agent is refused too.
    let request = lane.dir.path().join("detached-csv.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request(
            "app_record_csv_preview",
            json!({"install_id": install, "context_id": context_id, "csv_text": csv}),
        )
        .to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false);
    assert!(frame.to_string().contains("operator"));
    // The agent smuggled nothing in.
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_show",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-9"}),
            )
            .is_err(),
        "agent import smuggled a row"
    );
}

#[test]
fn cad779_csv_two_installations_and_two_contexts_isolate() {
    let w = Records::new();
    let first = w.install();
    let second = w.install_second();
    let a = first["install_id"].as_str().unwrap();
    let b = second["install_id"].as_str().unwrap();
    let ctx_a = w.context(a, "Client A", "ctx-a")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_a2 = w.context(a, "Client A2", "ctx-a2")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = w.context(b, "Client B", "ctx-b")["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The same customer id holds different people in each scope.
    w.create(a, &ctx_a, "customer-1", Records::profile(PROFILE_A));
    w.create(a, &ctx_a2, "customer-1", Records::profile(PROFILE_B));
    w.create(b, &ctx_b, "customer-1", Records::profile(PROFILE_B));

    // A preview scoped to A/ctx-a sees only its own rows: the sibling
    // email is unknown here, so a fresh id plans a create.
    let csv = "record_id,display_name,email\ncustomer-9,Boris Feld,boris@example.com\n";
    let preview = w.preview(a, &ctx_a, csv);
    assert_eq!(row(&preview, 1)["decision"], "create");
    assert!(row(&preview, 1).get("duplicate_of").is_none());

    // An import naming another installation's context is refused by the
    // live-context proof before any file is touched.
    let token = preview["preview_token"].as_str().unwrap().to_string();
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_import",
                json!({"install_id": a, "context_id": ctx_b, "csv_text": csv, "preview_token": token, "request_id": "req-x"}),
            )
            .is_err(),
        "cross-install import reached a file"
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": "no-such-install", "context_id": ctx_a, "csv_text": csv}),
            )
            .is_err(),
        "forged-install preview resolved"
    );
    assert_eq!(w.show(a, &ctx_a, "customer-1")["record"]["revision"], 1);
    assert_eq!(
        w.show(b, &ctx_b, "customer-1")["record"]["profile"]["display_name"],
        "Boris Feld"
    );
}

#[test]
fn cad779_csv_concurrent_import_with_one_request_id_applies_once() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap().to_string();
    let context = w.context(&install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap().to_string();
    let csv = "record_id,display_name,email\ncustomer-9,Chidi Anagonye,chidi@example.com\ncustomer-10,Elena Ruiz,elena@example.com\n"
        .to_string();

    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    // Retry while a sibling holds the same request id;
                    // every attempt converges on one stored receipt.
                    for _ in 0..40 {
                        let outcome = w.daemon.operator_rpc(
                            "app_record_csv_import",
                            json!({"install_id": install, "context_id": context_id, "csv_text": csv, "preview_token": w.daemon.operator_rpc("app_record_csv_preview", json!({"install_id": install, "context_id": context_id, "csv_text": csv})).unwrap()["preview_token"], "request_id": "req-race"}),
                        );
                        if let Ok(settled) = outcome {
                            return settled;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(25));
                    }
                    panic!("concurrent import never settled");
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    // One stored receipt: all threads converge on the same summary,
    // exactly one of them applied it.
    for result in &results {
        assert_eq!(
            result["summary"],
            json!({"applied": 2, "skipped": 0, "failed": 0})
        );
    }
    assert_eq!(
        results
            .iter()
            .filter(|result| result["replayed"] == false)
            .count(),
        1,
        "concurrent import applied {results:?}"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 2);
}

#[test]
fn cad779_customer_search_pagination_and_extended_profile() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    w.create(
        install,
        context_id,
        "customer-2",
        Records::profile(PROFILE_B),
    );
    let extended = json!({"schema": 1, "display_name": "Chidi Anagonye", "email": "chidi@example.com", "phone": "+1 555-0100", "tags": ["newcomer"], "source": "csv-import", "consent": {"email": "unknown"}});
    w.create(install, context_id, "customer-3", extended);

    // Search narrows to the matching record.
    let found = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id, "query": "amina"}),
        )
        .unwrap();
    assert_eq!(found["records"].as_array().unwrap().len(), 1);
    assert_eq!(found["records"][0]["id"], "customer-1");

    // Pagination pages the inventory with a stable cursor.
    let page_one = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id, "limit": 2}),
        )
        .unwrap();
    assert_eq!(page_one["records"].as_array().unwrap().len(), 2);
    assert_eq!(page_one["truncated"], true);
    let cursor = page_one["next_cursor"].as_str().unwrap().to_string();
    assert!(!cursor.is_empty());
    let page_two = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id, "limit": 2, "cursor": cursor}),
        )
        .unwrap();
    assert_eq!(page_two["records"].as_array().unwrap().len(), 1);
    assert_eq!(page_two["records"][0]["id"], "customer-3");

    // Bounds refuse: empty/huge limits, forged fields, control queries.
    for params in [
        json!({"install_id": install, "context_id": context_id, "limit": 0}),
        json!({"install_id": install, "context_id": context_id, "limit": 101}),
        json!({"install_id": install, "context_id": context_id, "query": "a\u{0}b"}),
        json!({"install_id": install, "context_id": context_id, "cursor": "../escape"}),
        json!({"install_id": install, "context_id": context_id, "by": "operator"}),
        json!({"install_id": install, "context_id": context_id, "project": "client"}),
    ] {
        assert!(
            w.daemon.operator_rpc("app_record_list", params).is_err(),
            "bounded list accepted an out-of-shape call"
        );
    }

    // The extended profile round-trips through show.
    let shown = w.show(install, context_id, "customer-3");
    assert_eq!(shown["record"]["profile"]["phone"], "+1 555-0100");
    assert_eq!(shown["record"]["profile"]["source"], "csv-import");
    // Consent history attributes each channel transition.
    let history = shown["record"]["consent_history"].as_array().unwrap();
    assert!(!history.is_empty(), "consent history missing: {shown}");
    let consent_change = json!({"schema": 1, "display_name": "Chidi Anagonye", "email": "chidi@example.com", "phone": "+1 555-0100", "tags": ["newcomer"], "source": "csv-import", "consent": {"email": "granted"}});
    w.daemon
        .operator_rpc(
            "app_record_update",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-3", "expected_revision": 1, "profile": consent_change}),
        )
        .unwrap();
    let after = w.show(install, context_id, "customer-3");
    assert!(
        after["record"]["consent_history"].as_array().unwrap().len() > history.len(),
        "consent change left no history: {after}"
    );

    // Bad contact fields refuse without echoing the marker.
    let marker = "cad779-private-profile-marker";
    for profile in [
        json!({"schema": 1, "display_name": marker, "phone": "not-a-phone", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "source": "has space", "consent": {"email": "granted"}}),
        json!({"schema": 1, "display_name": marker, "email": "a@b@c@d", "consent": {"email": "granted"}}),
    ] {
        let error = w
            .daemon
            .operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-bad", "profile": profile}),
            )
            .unwrap_err()
            .to_string();
        assert!(!error.contains(marker), "profile content leaked in refusal");
    }
}

#[test]
fn cad779_csv_corrupt_and_oversized_inputs_refuse_without_mutation() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.create(
        install,
        context_id,
        "customer-1",
        Records::profile(PROFILE_A),
    );
    let marker = "cad779-private-csv-marker";

    // Duplicate headers, unknown columns and corrupt quoting refuse whole.
    for csv in [
        "record_id,display_name,display_name\ncustomer-9,Name,Other\n".to_string(),
        "record_id,display_name,favorite_color\ncustomer-9,Name,blue\n".to_string(),
        "record_id,display_name,email\ncustomer-9,\"Unterminated Name,chidi@example.com\n"
            .to_string(),
    ] {
        let error = w
            .daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context_id, "csv_text": csv}),
            )
            .unwrap_err()
            .to_string();
        assert!(
            !error.contains(marker),
            "CSV content leaked in refusal: {error}"
        );
    }
    // A ragged row errors in place with a code, never the cell value.
    let ragged = format!(
        "record_id,display_name,email\ncustomer-9,{marker},chidi@example.com,extra-column\n"
    );
    let ragged_preview = w.preview(install, context_id, &ragged);
    assert_eq!(row(&ragged_preview, 1)["decision"], "error");
    assert!(!ragged_preview.to_string().contains(marker));
    // An oversized payload refuses before parsing.
    let big = format!(
        "record_id,display_name,email\ncustomer-9,{},chidi@example.com\n",
        "x".repeat(300 * 1024)
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context_id, "csv_text": big}),
            )
            .is_err(),
        "oversized CSV accepted"
    );
    // A 501-row payload refuses before applying anything.
    let mut rows = String::from("record_id,display_name,email\n");
    for n in 0..501 {
        rows.push_str(&format!("customer-{n:04},Name {n},user{n}@example.com\n"));
    }
    assert!(
        w.daemon
            .operator_rpc(
                "app_record_csv_preview",
                json!({"install_id": install, "context_id": context_id, "csv_text": rows}),
            )
            .is_err(),
        "over-row-limit CSV accepted"
    );
    // Nothing above mutated the file.
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 1);
}

#[test]
fn cad779_cli_csv_preview_and_import_roundtrip() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    let csv_path = w._root.path().join("customers.csv");
    std::fs::write(
        &csv_path,
        "record_id,display_name,email\ncustomer-9,Chidi Anagonye,chidi@example.com\n",
    )
    .unwrap();

    let previewed: Value = serde_json::from_slice(
        &common::operator_cadence_at(
            w._root.path(),
            &w.daemon.state,
            &[
                "app",
                "record",
                "csv-preview",
                install,
                "--context-id",
                context_id,
                "--csv",
                csv_path.to_str().unwrap(),
            ],
        )
        .stdout,
    )
    .unwrap();
    let token = previewed["preview_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("sha256:"));

    let output = common::operator_cadence_at(
        w._root.path(),
        &w.daemon.state,
        &[
            "app",
            "record",
            "csv-import",
            install,
            "--context-id",
            context_id,
            "--csv",
            csv_path.to_str().unwrap(),
            "--preview-token",
            &token,
            "--request-id",
            "req-cli",
        ],
    );
    assert!(
        output.status.success(),
        "csv import CLI: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let imported: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        imported["summary"],
        json!({"applied": 1, "skipped": 0, "failed": 0})
    );
    assert_eq!(
        w.show(install, context_id, "customer-9")["record"]["revision"],
        1
    );
}

struct Board {
    root: tempfile::TempDir,
    pm_dir: PathBuf,
    daemon: TestDaemon,
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<cadence_agent::Result<()>>>,
    install: String,
    context_id: String,
}

impl Board {
    fn new() -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let pm_dir = pm.dir.clone();
        let mut opts = daemon_opts();
        opts.test_seam = false;
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        Records::copy_source(&root.path().join("source"), "blog-post");
        let installed = daemon
            .operator_rpc(
                "app_workspace_install",
                json!({"source": root.path().join("source")}),
            )
            .unwrap();
        let install = installed["install_id"].as_str().unwrap().to_owned();
        let context = daemon
            .operator_rpc(
                "app_context_create",
                json!({"install_id": install, "label": "Client", "input_defaults": {}, "request_id": "ctx-csv-1"}),
            )
            .unwrap()["context"]
            .clone();
        let context_id = context["id"].as_str().unwrap().to_owned();
        let port = (3110..3200)
            .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let mut board = Self {
            root,
            pm_dir,
            daemon,
            port,
            stop,
            thread: None,
            install,
            context_id,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "csv board startup deadline exhausted"
            );
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = cadence_agent::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: board.port,
                stop: Some(board.stop.clone()),
                startup: Some(startup),
                test_seam: false,
                ..Default::default()
            };
            let state = board.daemon.state.clone();
            let pm_dir = board.pm_dir.clone();
            board.thread = Some(std::thread::spawn(move || {
                cadence_agent::ui::serve(&state, &pm_dir, &opts)
            }));
            let notification = match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) if !remaining.is_zero() => ready.recv_timeout(remaining),
                _ => Err(std::sync::mpsc::RecvTimeoutError::Timeout),
            };
            if matches!(notification, Ok(Ok(()))) {
                return board;
            }
            board.stop.store(true, Ordering::SeqCst);
            let result = board.thread.take().unwrap().join();
            if matches!(notification, Ok(Err(std::io::ErrorKind::AddrInUse))) {
                match result {
                    Ok(Err(error)) => eprintln!("csv board startup contention: {error}"),
                    unexpected => panic!("csv board bind failure returned {unexpected:?}"),
                }
                board.port = board
                    .port
                    .checked_add(1)
                    .filter(|port| *port < 3200)
                    .expect("csv board startup exhausted permitted ports");
                board.stop.store(false, Ordering::SeqCst);
            } else {
                panic!("csv board startup notification {notification:?}; worker {result:?}");
            }
        }
    }

    fn base(&self) -> String {
        format!(
            "/api/app-installations/{}/contexts/{}/records",
            self.install, self.context_id
        )
    }

    fn operator(&self, method: &str, path: &str, body: &str) -> (u16, String) {
        let session =
            common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, self.port);
        let (code, _, body) = common::op::raw(self.port, &session.request(method, path, body));
        (code, body)
    }

    fn value(&self, method: &str, path: &str, body: Value) -> Value {
        let encoded = body.to_string();
        let (code, result) = self.operator(method, path, &encoded);
        assert_eq!(code, 200, "operator csv request {method} {path}: {result}");
        serde_json::from_str(&result).unwrap()
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let result = thread.join();
            if std::thread::panicking() {
                if !matches!(&result, Ok(Ok(()))) {
                    eprintln!("csv board worker cleanup after primary panic: {result:?}");
                }
            } else {
                result.unwrap().unwrap();
            }
        }
    }
}

#[test]
fn cad779_http_csv_preview_import_roundtrip_matches_rpc() {
    let b = Board::new();
    let base = b.base();
    let csv = "record_id,display_name,email,tags,consent_email\ncustomer-9,Chidi Anagonye,chidi@example.com,newcomer,granted\n";
    let previewed = b.value(
        "POST",
        &format!("{base}/csv-preview"),
        json!({"csv_text": csv}),
    );
    let token = previewed["preview_token"].as_str().unwrap().to_string();
    assert!(token.starts_with("sha256:"));
    assert_eq!(previewed["summary"]["create"], 1);

    let imported = b.value(
        "POST",
        &format!("{base}/csv-import"),
        json!({"csv_text": csv, "preview_token": token, "request_id": "req-http"}),
    );
    assert_eq!(
        imported["summary"],
        json!({"applied": 1, "skipped": 0, "failed": 0})
    );
    // The HTTP receipt matches the daemon RPC receipt exactly.
    let via_rpc = b
        .daemon
        .operator_rpc(
            "app_record_show",
            json!({"install_id": b.install, "context_id": b.context_id, "record_id": "customer-9"}),
        )
        .unwrap();
    assert_eq!(
        via_rpc["record"]["profile"]["display_name"],
        "Chidi Anagonye"
    );
    // Search over HTTP narrows like RPC.
    let (code, text) = b.operator("GET", &base, "");
    assert_eq!(code, 200);
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed["records"].as_array().unwrap().len(), 1);
}

#[test]
fn cad779_http_csv_forged_verb_and_fields_refuse_without_leak() {
    let b = Board::new();
    let base = b.base();
    let marker = "cad779-private-http-marker";
    // A row error over HTTP names the code, never the cell value.
    let csv = format!("record_id,display_name,email\ncustomer-9,{marker},not-an-email\n");
    let (code, text) = b.operator(
        "POST",
        &format!("{base}/csv-preview"),
        &json!({"csv_text": csv}).to_string(),
    );
    assert_eq!(code, 200, "csv preview refused a row-error plan: {text}");
    let previewed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(previewed["rows"][0]["decision"], "error");
    assert!(
        !text.contains(marker),
        "preview echoed customer content: {text}"
    );

    // Forged body fields are refused by the exact transport grammar.
    for body in [
        json!({"csv_text": csv, "by": "operator"}),
        json!({"csv_text": csv, "actor": "operator"}),
        json!({"csv_text": csv, "project": "client"}),
        json!({"csv_text": csv, "install_id": b.install}),
        json!({"csv_text": csv, "context_id": b.context_id}),
        json!({"csv_text": csv, "record_id": "customer-9"}),
    ] {
        let (code, _) = b.operator("POST", &format!("{base}/csv-preview"), &body.to_string());
        assert_eq!(code, 400, "forged preview body accepted: {body}");
    }
    for body in [
        json!({"csv_text": csv, "preview_token": "sha256:x", "request_id": "req-f", "workspace": "default"}),
        json!({"csv_text": csv, "preview_token": "sha256:x", "request_id": "req-f", "project_link": "client"}),
    ] {
        let (code, _) = b.operator("POST", &format!("{base}/csv-import"), &body.to_string());
        assert_eq!(code, 400, "forged import body accepted: {body}");
    }
    // Reads stay reads: GET on the CSV routes is 405, queries are 400.
    assert_eq!(b.operator("GET", &format!("{base}/csv-preview"), "").0, 405);
    assert_eq!(b.operator("GET", &format!("{base}/csv-import"), "").0, 405);
    let (code, _) = b.operator("POST", &format!("{base}/csv-preview?x=1"), "{}");
    assert_eq!(code, 400, "query-bearing preview accepted");
    // Nothing above created a record.
    assert_eq!(b.operator("GET", &format!("{base}/customer-9"), "").0, 409);
}

#[test]
fn cad779_http_csv_cross_scope_and_unproven_callers_refuse() {
    let b = Board::new();
    let base = b.base();
    let csv = "record_id,display_name,email\ncustomer-9,Chidi Anagonye,chidi@example.com\n";
    let previewed = b.value(
        "POST",
        &format!("{base}/csv-preview"),
        json!({"csv_text": csv}),
    );
    let token = previewed["preview_token"].as_str().unwrap().to_string();

    // A second installation with its own context.
    let second_source = b.root.path().join("second");
    for name in [
        "app.md",
        "workflows/blog-post.md",
        "rubrics/blog.md",
        "templates/brief.md",
        "templates/post.md",
    ] {
        let destination = second_source.join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("apps/blog-post")
                .join(name),
            &destination,
        )
        .unwrap();
    }
    let manifest = second_source.join("app.md");
    let text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(
        manifest,
        text.replace("app: blog-post", "app: blog-post-two"),
    )
    .unwrap();
    let second = b
        .daemon
        .operator_rpc("app_workspace_install", json!({"source": second_source}))
        .unwrap();
    let install_b = second["install_id"].as_str().unwrap().to_string();
    let ctx_b = b
        .daemon
        .operator_rpc(
            "app_context_create",
            json!({"install_id": install_b, "label": "Client B", "input_defaults": {}, "request_id": "ctx-csv-b"}),
        )
        .unwrap()["context"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Cross-install imports fail over HTTP exactly like RPC.
    let other_import =
        format!("/api/app-installations/{install_b}/contexts/{ctx_b}/records/csv-import");
    let (code, _) = b.operator(
        "POST",
        &other_import,
        &json!({"csv_text": csv, "preview_token": token, "request_id": "req-x"}).to_string(),
    );
    assert_ne!(code, 200, "cross-install HTTP import admitted");
    // Forged scopes fail closed.
    let forged = format!(
        "/api/app-installations/no-such-install/contexts/{}/records/csv-preview",
        b.context_id
    );
    assert_ne!(
        b.operator("POST", &forged, &json!({"csv_text": csv}).to_string())
            .0,
        200
    );

    // An agent replay without caller assertion is refused over HTTP.
    let mut lane = LaneShell::spawn(b.root.path());
    plant_member_pane(&b.daemon, "csv-http-worker", "claude", None, lane.pid());
    let stolen = common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &b.daemon.state, b.port);
    let wire = stolen.request_as(
        "POST",
        &format!("{base}/csv-preview"),
        &json!({"csv_text": csv}).to_string(),
        "",
    );
    assert!(!wire.contains(cadence_agent::test_seam::AS_HEADER));
    let file = lane.dir.path().join("csv-request.txt");
    std::fs::write(&file, wire).unwrap();
    let (rc, response) = lane.run(&format!("python3 -c 'import socket,sys;s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));s.sendall(open(sys.argv[2],\"rb\").read());print(s.makefile().readline())' {} {}", b.port, file.display()));
    assert_eq!(rc, 0);
    assert!(
        response.contains(" 403 "),
        "unasserted HTTP preview admitted: {response}"
    );
    // A sessionless import is refused too.
    let host = common::op::board_host(b.port);
    let body =
        json!({"csv_text": csv, "preview_token": token, "request_id": "req-bare"}).to_string();
    let bare = format!(
        "POST {base}/csv-import HTTP/1.0\r\nHost: {host}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let (code, _, _) = common::op::raw(b.port, &bare);
    assert_eq!(code, 403, "sessionless import admitted");
    // Nothing above imported a row.
    assert_eq!(b.operator("GET", &format!("{base}/customer-9"), "").0, 409);
}

#[test]
fn cad779_csv_concurrent_imports_conflict_on_email_without_second_row() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    // Two different imports plan a create for two different record
    // IDs behind one address. Both previews run before either import
    // commits, so both plans say create: the apply-time guard must
    // still leave exactly one live row.
    let csv_a = "record_id,display_name,email\ncustomer-race-a,Racer A,race@example.com\n";
    let csv_b = "record_id,display_name,email\ncustomer-race-b,Racer B,race@example.com\n";
    let token_a = w.preview(install, context_id, csv_a)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();
    let token_b = w.preview(install, context_id, csv_b)["preview_token"]
        .as_str()
        .unwrap()
        .to_string();
    let barrier = std::sync::Barrier::new(2);
    let (left, right) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            w.daemon.operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context_id, "csv_text": csv_a, "preview_token": token_a, "request_id": "req-race-a"}),
            )
        });
        let second = scope.spawn(|| {
            barrier.wait();
            w.daemon.operator_rpc(
                "app_record_csv_import",
                json!({"install_id": install, "context_id": context_id, "csv_text": csv_b, "preview_token": token_b, "request_id": "req-race-b"}),
            )
        });
        (
            first.join().unwrap().unwrap(),
            second.join().unwrap().unwrap(),
        )
    });
    // Exactly one create wins across both receipts. The loser either
    // fails at apply time with a duplicate receipt (its plan predates
    // the winner's commit) or skips conservatively at plan time (its
    // plan already sees the winner): the contract guarantees one live
    // row and a recoverable loser receipt either way.
    let applied = left["summary"]["applied"].as_i64().unwrap()
        + right["summary"]["applied"].as_i64().unwrap();
    assert_eq!(applied, 1, "email race planted two rows: {left} {right}");
    for result in [&left, &right] {
        let rows = result["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "receipt lost its row: {result}");
        if result["summary"]["applied"].as_i64().unwrap() == 0 {
            let outcome = rows[0]["outcome"].as_str().unwrap();
            assert!(
                (outcome == "failed" && rows[0]["reason"] == "duplicate email")
                    || outcome == "skipped",
                "loser left no recoverable receipt: {result}"
            );
        }
    }
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    let records = listed["records"].as_array().unwrap();
    assert_eq!(records.len(), 1, "second live row survived: {listed}");
    assert_eq!(
        records[0]["profile"]["email"], "race@example.com",
        "winner row corrupted: {listed}"
    );
}

#[test]
fn cad779_concurrent_creates_refuse_same_normalized_email_at_write_boundary() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Write boundary", "ctx-email-write-race");
    let context_id = context["id"].as_str().unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (left, right) = std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            barrier.wait();
            w.daemon.operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-email-a", "profile": {"schema": 1, "display_name": "First", "email": "RACE@example.com", "tags": [], "consent": {"email": "unknown"}}}),
            )
        });
        let second = scope.spawn(|| {
            barrier.wait();
            w.daemon.operator_rpc(
                "app_record_create",
                json!({"install_id": install, "context_id": context_id, "record_id": "customer-email-b", "profile": {"schema": 1, "display_name": "Second", "email": "race@example.com", "tags": [], "consent": {"email": "unknown"}}}),
            )
        });
        (first.join().unwrap(), second.join().unwrap())
    });
    let outcomes = [&left, &right];
    assert_eq!(
        outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
        1,
        "two IDs acquired the same normalized email: {outcomes:?}"
    );
    let refusal = outcomes
        .iter()
        .find_map(|outcome| outcome.as_ref().err())
        .unwrap()
        .to_string();
    assert!(
        refusal.contains("another record"),
        "loser was not refused for the held email: {refusal}"
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(
        listed["records"].as_array().unwrap().len(),
        1,
        "two live rows survived the write race: {listed}"
    );
}

#[test]
fn cad779_csv_blank_record_id_is_a_row_error() {
    let w = Records::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    // A blank ID is refused as a row error — never derived, never
    // persisted — while the valid sibling still plans a create.
    let csv = "record_id,display_name,email\n,No Id,noid@example.com\ncustomer-8,Has Id,hasid@example.com\n";
    let preview = w.preview(install, context_id, csv);
    let blank = row(&preview, 1);
    assert_eq!(blank["decision"], "error");
    assert!(
        blank["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|code| code == "record id"),
        "blank id refused without its code: {preview}"
    );
    assert_eq!(row(&preview, 2)["decision"], "create");
    let token = preview["preview_token"].as_str().unwrap().to_string();
    let imported = w.import(install, context_id, csv, &token, "req-blank");
    assert_eq!(
        imported["summary"],
        json!({"applied": 1, "skipped": 1, "failed": 0})
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_record_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    let ids: Vec<&str> = listed["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| record["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["customer-8"], "derived id persisted: {listed}");
}
