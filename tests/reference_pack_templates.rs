//! CAD-1463: native CLI/RPC reference-pack lifecycle and safe consumption.
//! Caller assertions use the existing isolated test seam, never production identity.
#![cfg(all(unix, feature = "test-seam"))]

use base64::{engine::general_purpose::STANDARD, Engine};
use cadence_agent::test_seam::{scoped, Asserted, AS_ENV};
use cadence_agent::{adapter::ProviderEnv, client, daemon, issue::Pm, Error};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_cadence");
const GUIDE: &str = "Use a restrained editorial hierarchy: large left-aligned serif title, short narrative, a narrow green rule and generous margins. Keep request facts separate; inspect rendered references, not only this description.";
const REFERENCE: &str = "Reference composition: upper-left headline, two-thirds narrative, one-third evidence. Learn hierarchy; do not reuse another customer's claims.";

struct Fixture {
    root: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    daemon: Option<JoinHandle<cadence_agent::Result<()>>>,
}

impl Fixture {
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("c1463-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in [
            "home", "xdg", "config", "data", "cache", "tmp", "locks", "state",
        ] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        for (key, dir) in [
            ("HOME", "home"),
            ("XDG_STATE_HOME", "xdg"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            std::env::set_var(key, root.path().join(dir));
        }
        std::env::set_var("TMPDIR", root.path());
        std::env::set_var("CADENCE_SUITE_LOCK", root.path().join("locks/suite"));
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let provider_env = ProviderEnv::refusing_providers();
        provider_env.set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let state = root.path().join("state");
        let stop = Arc::new(AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            provider_env,
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
        let run_state = state.clone();
        let daemon = thread::spawn(move || daemon::serve_with(&run_state, opts));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if client::rpc_timeout(&state, "health", json!({}), Duration::from_secs(1)).is_ok() {
                break;
            }
            assert!(
                !daemon.is_finished() && Instant::now() < deadline,
                "fixture daemon failed to start"
            );
            thread::sleep(Duration::from_millis(50));
        }
        Self {
            root,
            stop,
            daemon: Some(daemon),
        }
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .current_dir(self.root.path())
            .arg("--state-dir")
            .arg(self.root.path().join("state"))
            .args(args)
            .env("PATH", "/usr/bin:/bin")
            .env(AS_ENV, "operator")
            .env("HOME", self.root.path().join("home"))
            .env("XDG_STATE_HOME", self.root.path().join("xdg"))
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env("TMPDIR", self.root.path())
            .env("CADENCE_SUITE_LOCK", self.root.path().join("locks/suite"))
            .stdin(Stdio::null());
        for key in ["CARGO_HOME", "RUSTUP_HOME"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        cadence_agent::reaper::output(&mut command).unwrap()
    }

    fn success(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> cadence_agent::Result<Value> {
        scoped(who, || {
            client::rpc(&self.root.path().join("state"), method, params)
        })
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.daemon.take() {
            let _ = handle.join();
        }
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn pack() -> Value {
    json!({"kind":"authoring", "purpose":"Reusable proposal style for a fictional company", "format":"html", "guidelines":GUIDE,
        "references":[{"name":"layout.txt", "media_type":"text/plain", "description":"Study hierarchy and content balance; no request facts are defaults.", "content_base64":STANDARD.encode(REFERENCE)}]})
}

#[test]
fn native_reference_pack_versions_and_materialisation_preserve_authority_and_content() {
    let fixture = Fixture::start();
    let input = fixture.root.path().join("pack.json");
    write_json(&input, &pack());
    let created = fixture.success(&[
        "template",
        "create",
        "editorial",
        "--file",
        input.to_str().unwrap(),
    ]);
    assert_eq!(created["template"]["version"], 1);
    assert_eq!(created["template"]["status"], "draft");
    assert_eq!(created["spec"]["guidelines"], GUIDE);
    let digest = created["template"]["content_sha256"].as_str().unwrap();

    let forbidden = fixture
        .rpc(
            Asserted::Agent("ordinary".into()),
            "business_template_approve",
            json!({"name":"editorial", "version":1, "digest":digest, "source":"not an operator"}),
        )
        .unwrap_err();
    assert!(matches!(forbidden, Error::Rejected(_)), "{forbidden:?}");
    let wrong_scope = fixture
        .rpc(
            Asserted::Operator,
            "business_template_show",
            json!({"name":"editorial", "version":1, "company_id":"another-company"}),
        )
        .unwrap_err();
    assert!(matches!(wrong_scope, Error::Rejected(_)), "{wrong_scope:?}");
    let unproven = fixture
        .rpc(
            Asserted::Unproven,
            "business_template_show",
            json!({"name":"editorial", "version":1}),
        )
        .unwrap_err();
    assert!(matches!(unproven, Error::Rejected(_)), "{unproven:?}");

    let approved = fixture.success(&[
        "template",
        "approve",
        "editorial",
        "--version",
        "1",
        "--sha256",
        digest,
        "--source",
        "synthetic fixture approval, not human business approval",
    ]);
    assert_eq!(approved["template"]["status"], "approved");
    let output = fixture.root.path().join("resolved");
    fixture.success(&[
        "template",
        "show",
        "editorial",
        "--version",
        "1",
        "--out",
        output.to_str().unwrap(),
    ]);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        fs::metadata(&output).unwrap().permissions().mode() & 0o077,
        0
    );
    assert_eq!(
        fs::read_to_string(output.join("guidelines.md")).unwrap(),
        GUIDE
    );
    assert_eq!(
        fs::read_to_string(output.join("layout.txt")).unwrap(),
        REFERENCE
    );
    let manifest: Value =
        serde_json::from_slice(&fs::read(output.join("template.json")).unwrap()).unwrap();
    assert_eq!(manifest["template"]["content_sha256"], digest);
    assert_eq!(manifest["template"]["status"], "approved");

    fs::write(output.join("layout.txt"), "user local annotations").unwrap();
    assert!(!fixture
        .cli(&[
            "template",
            "show",
            "editorial",
            "--version",
            "1",
            "--out",
            output.to_str().unwrap()
        ])
        .status
        .success());
    assert_eq!(
        fs::read_to_string(output.join("layout.txt")).unwrap(),
        "user local annotations"
    );
    let external = fixture.root.path().join("private");
    fs::create_dir(&external).unwrap();
    fs::write(external.join("guidelines.md"), "private content").unwrap();
    let link = fixture.root.path().join("linked");
    std::os::unix::fs::symlink(&external, &link).unwrap();
    assert!(!fixture
        .cli(&[
            "template",
            "show",
            "editorial",
            "--version",
            "1",
            "--out",
            link.to_str().unwrap()
        ])
        .status
        .success());
    assert_eq!(
        fs::read_to_string(external.join("guidelines.md")).unwrap(),
        "private content"
    );

    let mut next = pack();
    next["guidelines"] = json!(format!(
        "{GUIDE}\nUse indigo, not green, for the accent rule."
    ));
    next["references"][0]["content_base64"] = json!(STANDARD.encode(
        "Revised reference: top-left title, generous whitespace and an indigo accent rule."
    ));
    write_json(&input, &next);
    let revised = fixture.success(&[
        "template",
        "revise",
        "editorial",
        "--base",
        "1",
        "--file",
        input.to_str().unwrap(),
    ]);
    assert_eq!(revised["template"]["version"], 2);
    assert_eq!(revised["template"]["status"], "draft");
    assert_ne!(revised["template"]["content_sha256"], digest);
    let old = fixture.success(&["template", "show", "editorial", "--version", "1"]);
    assert_eq!(old["spec"], pack());
    assert_eq!(old["template"]["content_sha256"], digest);
    let stale_approval = fixture.rpc(Asserted::Operator, "business_template_approve", json!({"name":"editorial", "version":2, "digest":digest, "source":"synthetic stale approval"}));
    assert!(matches!(stale_approval, Err(Error::Rejected(_))));

    let mut bad = pack();
    bad["references"][0]["name"] = json!("../private.txt");
    let rejected = fixture.rpc(
        Asserted::Operator,
        "business_template_create",
        json!({"name":"unsafe", "spec":bad}),
    );
    assert!(matches!(rejected, Err(Error::Rejected(_))));
    let absent = fixture
        .rpc(
            Asserted::Operator,
            "business_template_show",
            json!({"name":"unsafe"}),
        )
        .unwrap();
    assert_eq!(absent["versions"], json!([]));

    let sample = fixture.rpc(
        Asserted::Operator,
        "business_template_sample",
        json!({"name":"editorial", "version":1}),
    );
    assert!(matches!(sample, Err(Error::Rejected(_))));
    let financial = fixture.rpc(Asserted::Operator, "business_document_create", json!({"template":"editorial", "version":1, "data":{"items":[{"description":"fake line", "quantity":"1", "unit_price":"100"}]}}));
    assert!(matches!(financial, Err(Error::Rejected(_))));

    // Financial definitions retain their old canonical bytes and approval
    // digests. Both financial kinds still create calculated drafts, not issues.
    let source_base64 = include_str!("fixtures/template-financial.docx.b64").trim();
    let source_bytes = STANDARD.decode(source_base64).unwrap();
    let source_sha = cadence_agent::business_templates::sha256_hex(&source_bytes);
    for kind in ["quotation", "invoice"] {
        let spec = json!({"kind":kind, "fields":[{"key":"customer","label":"Customer","kind":"text","required":true}], "fixed_text":{},
            "calculation":{"confirmed":true,"currency":"GBP","tax_rate_bps":1000,"discount_bps":0}});
        let legacy_json = format!("{{\"kind\":\"{kind}\",\"fields\":[{{\"key\":\"customer\",\"label\":\"Customer\",\"kind\":\"text\",\"required\":true}}],\"fixed_text\":{{}},\"calculation\":{{\"confirmed\":true,\"currency\":\"GBP\",\"tax_rate_bps\":1000,\"discount_bps\":0}}}}");
        let legacy_digest = cadence_agent::business_templates::sha256_hex(
            format!("cad1393-template-v1\n{source_sha}\n{legacy_json}").as_bytes(),
        );
        let row = fixture
            .rpc(
                Asserted::Operator,
                "business_template_create",
                json!({"name":kind,"source":source_base64,"spec":spec}),
            )
            .unwrap();
        assert_eq!(row["template"]["content_sha256"], legacy_digest);
        fixture.rpc(Asserted::Operator, "business_template_approve", json!({"name":kind,"version":1,"digest":legacy_digest,"source":"synthetic regression fixture"})).unwrap();
        let draft = fixture
            .rpc(
                Asserted::Operator,
                "business_document_create",
                json!({"template":kind,"version":1,
            "data":{"fields":{"customer":"Fictional Harbour Learning Lab"},"items":[
                {"description":"Bench","quantity":"3","unit_price":"120.00"},
                {"description":"Kit","quantity":"2","unit_price":"75.00"}]}}),
            )
            .unwrap();
        assert_eq!(draft["state"], "draft");
        assert_eq!(draft["kind"], kind);
        assert_eq!(draft["context"]["subtotal"], "510.00");
        assert_eq!(draft["context"]["tax"], "51.00");
        assert_eq!(draft["context"]["total"], "561.00");
    }
}
