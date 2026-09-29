//! CAD-780 saved segments and frozen audiences over CAD-779 customers.
//!
//! Adversarial-first at the real daemon socket: an operator caller
//! gets typed segment/exclusion/suppression/audience actions with
//! revision CAS and frozen digests; an agent caller, a detached
//! child, forged `by`/`actor`/`project`/`project_link`/`workspace`
//! fields, cross-install and cross-context probes (reads included —
//! every action proves the live context), and stale/concurrent
//! writes are all refused without mutation.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

const PROFILE_A: &str = r#"{"schema":1,"display_name":"Amina Diallo","email":"amina@example.com","tags":["vip"],"consent":{"email":"granted"}}"#;
const PROFILE_B: &str = r#"{"schema":1,"display_name":"Boris Feld","email":"boris@example.com","tags":[],"consent":{"email":"denied"}}"#;
const PROFILE_C: &str = r#"{"schema":1,"display_name":"Cleo Boone","email":"cleo@example.com","tags":[],"consent":{"email":"granted"}}"#;

const VIP: &str = r#"[{"field":"tag","op":"eq","value":"vip"}]"#;

struct Audiences {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Audiences {
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

    fn seed(&self, install: &str, context: &str) {
        for (id, profile) in [
            ("customer-a", PROFILE_A),
            ("customer-b", PROFILE_B),
            ("customer-c", PROFILE_C),
        ] {
            self.daemon
                .operator_rpc(
                    "app_record_create",
                    json!({"install_id": install, "context_id": context, "record_id": id, "profile": Self::profile(profile)}),
                )
                .unwrap();
        }
    }

    fn predicates(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn seg_save(&self, install: &str, context: &str, seg: &str, rev: Option<u64>) -> Value {
        let mut params = json!({"install_id": install, "context_id": context, "segment_id": seg, "name": "VIP", "predicates": Self::predicates(VIP)});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        self.daemon
            .operator_rpc("app_segment_save", params)
            .unwrap()
    }

    fn excl_save(&self, install: &str, context: &str, list: &str, rev: Option<u64>) -> Value {
        let mut params = json!({"install_id": install, "context_id": context, "list_id": list, "name": "Hold", "member_ids": ["customer-c"]});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        self.daemon
            .operator_rpc("app_exclusion_save", params)
            .unwrap()
    }

    fn preview(&self, install: &str, context: &str, base: Value) -> Value {
        self.daemon
            .operator_rpc(
                "app_audience_preview",
                json!({"install_id": install, "context_id": context, "base": base}),
            )
            .unwrap()
    }
}

#[test]
fn cad780_operator_audience_roundtrip_with_frozen_digest() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.seed(install, context_id);

    let saved = w.seg_save(install, context_id, "seg-vip", None);
    assert_eq!(saved["segment"]["revision"], 1);
    assert!(saved["segment"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let shown = w
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip"}),
        )
        .unwrap();
    assert_eq!(shown["segment"], saved["segment"]);
    let listed = w
        .daemon
        .operator_rpc(
            "app_segment_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["segments"].as_array().unwrap().len(), 1);

    let excl = w.excl_save(install, context_id, "ex-hold", None);
    assert_eq!(excl["exclusion"]["revision"], 1);

    w.daemon
        .operator_rpc(
            "app_suppression_add",
            json!({"install_id": install, "context_id": context_id, "email": "cleo@example.com", "reason": "bounce"}),
        )
        .unwrap();
    let suppressions = w
        .daemon
        .operator_rpc(
            "app_suppression_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(suppressions["suppressions"].as_array().unwrap().len(), 1);

    // All: three rows minus denied minus suppressed = one.
    let all = w.preview(install, context_id, json!({"mode": "all"}));
    assert_eq!(all["base_count"], 3);
    assert_eq!(all["final_count"], 1);
    // Segment: only a carries the vip tag, and a is eligible.
    let segment = w.preview(
        install,
        context_id,
        json!({"mode": "segment", "segment_id": "seg-vip"}),
    );
    assert_eq!(segment["base_count"], 1);
    assert_eq!(segment["final_count"], 1);
    // Custom with duplicates dedupes; the exclusion list applies.
    let custom = w
        .daemon
        .operator_rpc(
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "custom", "customer_ids": ["customer-a", "customer-a", "customer-c"]}, "exclusion_list_id": "ex-hold"}),
        )
        .unwrap();
    assert_eq!(custom["base_count"], 2);
    assert_eq!(custom["exclusion_count"], 1);
    assert_eq!(custom["final_count"], 1);

    let prepared = w
        .daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50}),
        )
        .unwrap();
    assert_eq!(prepared["freeze"]["final_count"], 1);
    assert!(prepared["freeze"]["digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    let shown = w
        .daemon
        .operator_rpc(
            "app_audience_show",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1"}),
        )
        .unwrap();
    assert_eq!(shown["valid"], true);
    assert_eq!(shown["freeze"]["digest"], prepared["freeze"]["digest"]);

    // Suppression removal re-opens the audience only for a new freeze;
    // the old one stays valid because its pins still match.
    w.daemon
        .operator_rpc(
            "app_suppression_remove",
            json!({"install_id": install, "context_id": context_id, "email": "cleo@example.com"}),
        )
        .unwrap();
    let after = w.preview(install, context_id, json!({"mode": "all"}));
    assert_eq!(after["final_count"], 2);
}

#[test]
fn cad780_cross_install_and_cross_context_refuse_on_every_action() {
    let w = Audiences::new();
    let first = w.install();
    let second = w.install_second();
    let a = first["install_id"].as_str().unwrap();
    let b = second["install_id"].as_str().unwrap();
    let ctx_a = w.context(a, "Client A", "ctx-a")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_b = w.context(b, "Client B", "ctx-b")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let ctx_a2 = w.context(a, "Client A2", "ctx-a2")["id"]
        .as_str()
        .unwrap()
        .to_string();
    w.seed(a, &ctx_a);
    w.seg_save(a, &ctx_a, "seg-vip", None);
    w.excl_save(a, &ctx_a, "ex-hold", None);
    let before = w.preview(a, &ctx_a, json!({"mode": "all"}));

    // Mismatched and forged scopes refuse on every read — reads
    // prove the live context, so a foreign context fails closed.
    let reads: Vec<(&str, Value)> = vec![
        ("app_segment_show", json!({"segment_id": "seg-vip"})),
        ("app_segment_list", json!({})),
        ("app_exclusion_show", json!({"list_id": "ex-hold"})),
        ("app_exclusion_list", json!({})),
        ("app_suppression_list", json!({})),
        ("app_audience_preview", json!({"base": {"mode": "all"}})),
        ("app_audience_show", json!({"freeze_id": "freeze-1"})),
    ];
    for scope in [
        json!({"install_id": b, "context_id": ctx_a}),
        json!({"install_id": a, "context_id": ctx_b}),
        json!({"install_id": "no-such-install", "context_id": ctx_a}),
        json!({"install_id": a, "context_id": "ctx-no-such-context"}),
    ] {
        for (method, shape) in &reads {
            let mut params = scope.clone();
            for (key, value) in shape.as_object().unwrap() {
                params[key] = value.clone();
            }
            assert!(
                w.daemon.operator_rpc(method, params.clone()).is_err(),
                "cross-scope read reached {method}: {params}"
            );
        }
    }
    // Fully valid but unrelated scopes succeed empty: installation
    // B and the sibling context hold none of A's rows — isolation
    // is emptiness, not refusal, and nothing leaks across.
    for (install, context) in [(b, &ctx_b), (a, &ctx_a2)] {
        let listed = w
            .daemon
            .operator_rpc(
                "app_segment_list",
                json!({"install_id": install, "context_id": context}),
            )
            .unwrap();
        assert!(listed["segments"].as_array().unwrap().is_empty());
        assert!(w
            .daemon
            .operator_rpc(
                "app_segment_show",
                json!({"install_id": install, "context_id": context, "segment_id": "seg-vip"}),
            )
            .is_err());
        let preview = w
            .daemon
            .operator_rpc(
                "app_audience_preview",
                json!({"install_id": install, "context_id": context, "base": {"mode": "all"}}),
            )
            .unwrap();
        assert_eq!(preview["final_count"], 0);
    }
    // Crossed writes refuse: installation A never proves B's
    // context, so nothing lands in either file.
    for (method, shape) in [
        (
            "app_segment_save",
            json!({"segment_id": "seg-x", "name": "X", "predicates": Audiences::predicates(VIP)}),
        ),
        (
            "app_exclusion_save",
            json!({"list_id": "ex-x", "name": "X", "member_ids": []}),
        ),
        (
            "app_suppression_add",
            json!({"email": "x@example.com", "reason": "x"}),
        ),
        ("app_suppression_remove", json!({"email": "x@example.com"})),
        (
            "app_audience_prepare",
            json!({"freeze_id": "freeze-x", "base": {"mode": "all"}, "max_recipients": 50}),
        ),
    ] {
        let mut params = json!({"install_id": a, "context_id": ctx_b});
        for (key, value) in shape.as_object().unwrap() {
            params[key] = value.clone();
        }
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "cross-install write reached {method}"
        );
    }
    // Nothing above mutated installation A.
    assert_eq!(w.preview(a, &ctx_a, json!({"mode": "all"})), before);
    assert!(w
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": a, "context_id": ctx_a2, "segment_id": "seg-vip"}),
        )
        .is_err());
}

#[test]
fn cad780_reads_prove_the_active_context() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.seed(install, context_id);
    w.seg_save(install, context_id, "seg-vip", None);

    // Unknown contexts refuse on every read path.
    for (method, params) in [
        (
            "app_segment_show",
            json!({"install_id": install, "context_id": "ctx-no-such-context", "segment_id": "seg-vip"}),
        ),
        (
            "app_segment_list",
            json!({"install_id": install, "context_id": "ctx-no-such-context"}),
        ),
        (
            "app_exclusion_list",
            json!({"install_id": install, "context_id": "ctx-no-such-context"}),
        ),
        (
            "app_suppression_list",
            json!({"install_id": install, "context_id": "ctx-no-such-context"}),
        ),
        (
            "app_audience_preview",
            json!({"install_id": install, "context_id": "ctx-no-such-context", "base": {"mode": "all"}}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "unknown-context read admitted by {method}"
        );
    }
    // An archived context is no longer live: reads refuse too.
    let revision = context["revision"].as_u64().unwrap();
    w.daemon
        .operator_rpc(
            "app_context_archive",
            json!({"install_id": install, "context_id": context_id, "expected_revision": revision}),
        )
        .unwrap();
    for (method, params) in [
        (
            "app_segment_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "all"}}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "archived-context read admitted by {method}"
        );
    }
}

#[test]
fn cad780_stale_and_concurrent_segment_edits_refuse_without_mutation() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    let created = w.seg_save(install, context_id, "seg-vip", None);

    // Blind and stale overwrites refuse and change nothing.
    for rev in [None, Some(7)] {
        let mut params = json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "name": "Racer", "predicates": Audiences::predicates(VIP)});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        assert!(
            w.daemon.operator_rpc("app_segment_save", params).is_err(),
            "stale segment save accepted"
        );
    }
    let shown = w
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip"}),
        )
        .unwrap();
    assert_eq!(shown["segment"], created["segment"]);

    // Concurrent saves at the same observed revision: exactly one wins.
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    w.daemon
                        .operator_rpc(
                            "app_segment_save",
                            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "name": "Racer", "predicates": Audiences::predicates(VIP), "expected_revision": 1}),
                        )
                        .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        results.iter().filter(|ok| **ok).count(),
        1,
        "concurrent segment CAS admitted {results:?}"
    );
    let settled = w
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip"}),
        )
        .unwrap();
    assert_eq!(settled["segment"]["revision"], 2);
}

#[test]
fn cad780_agent_forged_and_detached_callers_cannot_touch_audiences() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.seed(install, context_id);
    w.seg_save(install, context_id, "seg-vip", None);
    let before = w.preview(install, context_id, json!({"mode": "all"}));

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "audience-worker", "claude", None, lane.pid());
    let calls: Vec<(&str, Value)> = vec![
        (
            "app_segment_save",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-evil", "name": "E", "predicates": Audiences::predicates(VIP)}),
        ),
        (
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip"}),
        ),
        (
            "app_segment_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_exclusion_save",
            json!({"install_id": install, "context_id": context_id, "list_id": "ex-evil", "name": "E", "member_ids": []}),
        ),
        (
            "app_exclusion_show",
            json!({"install_id": install, "context_id": context_id, "list_id": "ex-hold"}),
        ),
        (
            "app_exclusion_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_suppression_add",
            json!({"install_id": install, "context_id": context_id, "email": "evil@example.com", "reason": "x"}),
        ),
        (
            "app_suppression_remove",
            json!({"install_id": install, "context_id": context_id, "email": "evil@example.com"}),
        ),
        (
            "app_suppression_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "all"}}),
        ),
        (
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-evil", "base": {"mode": "all"}, "max_recipients": 50}),
        ),
        (
            "app_audience_show",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1"}),
        ),
    ];
    for (method, params) in &calls {
        let frame = lane.rpc(&w.daemon.state, method, params.clone());
        assert_eq!(frame["ok"], false, "agent reached {method}");
        assert!(
            frame.to_string().contains("operator"),
            "agent refusal missed caller authority for {method}: {frame}"
        );
    }
    // Forged identity and discovery-link fields never confer access.
    for params in [
        json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "by": "operator", "actor": "operator"}),
        json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "project": "client", "project_link": "client", "workspace": "default"}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_segment_show", params);
        assert_eq!(
            frame["ok"], false,
            "forged fields reached a segment: {frame}"
        );
    }
    // A detached child of the agent — no provable identity — is refused too.
    let request = lane.dir.path().join("detached.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request(
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "all"}}),
        )
        .to_string(),
    )
    .unwrap();
    let (rc, output) = lane.run(&format!("setsid python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(open(sys.argv[2],\"rb\").read()+b\"\\n\");print(s.makefile().readline())' {} {}", cadence_agent::client::socket_path(&w.daemon.state).display(), request.display()));
    assert_eq!(rc, 0);
    let frame: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(frame["ok"], false);
    assert!(frame.to_string().contains("operator"));

    // The lane is an enrolled caller and nothing mutated.
    assert_eq!(
        w.preview(install, context_id, json!({"mode": "all"})),
        before
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_segment_show",
                json!({"install_id": install, "context_id": context_id, "segment_id": "seg-evil"}),
            )
            .is_err(),
        "agent smuggled a segment"
    );
}

#[test]
fn cad780_operator_forged_fields_refuse_with_valid_control() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.seed(install, context_id);

    // Valid controls: the exact same shapes without forged fields work.
    let created = w.seg_save(install, context_id, "seg-vip", None);
    assert_eq!(created["segment"]["revision"], 1);
    let excl = w.excl_save(install, context_id, "ex-hold", None);
    assert_eq!(excl["exclusion"]["revision"], 1);

    // Forged identity fields refuse for the operator caller as well.
    for (method, params) in [
        (
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "by": "operator"}),
        ),
        (
            "app_segment_list",
            json!({"install_id": install, "context_id": context_id, "actor": "operator"}),
        ),
        (
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "all"}, "by": "operator"}),
        ),
        (
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "actor": "operator"}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "operator forged identity field accepted by {method}"
        );
    }
    // Discovery-link and routing fields refuse by the exact payload
    // allowlist — the connection gate does not know them.
    for (method, params) in [
        (
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip", "project": "client"}),
        ),
        (
            "app_segment_list",
            json!({"install_id": install, "context_id": context_id, "project_link": "client"}),
        ),
        (
            "app_exclusion_save",
            json!({"install_id": install, "context_id": context_id, "list_id": "ex-2", "name": "X", "member_ids": [], "workspace": "default"}),
        ),
        (
            "app_suppression_add",
            json!({"install_id": install, "context_id": context_id, "email": "x@example.com", "reason": "x", "project": "client"}),
        ),
        (
            "app_audience_preview",
            json!({"install_id": install, "context_id": context_id, "base": {"mode": "all"}, "project_link": "client"}),
        ),
        (
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50, "workspace": "default"}),
        ),
        (
            "app_audience_show",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "by": "operator", "project": "client"}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "operator forged link field accepted by {method}"
        );
    }
    // Nothing above mutated the file.
    let shown = w
        .daemon
        .operator_rpc(
            "app_segment_show",
            json!({"install_id": install, "context_id": context_id, "segment_id": "seg-vip"}),
        )
        .unwrap();
    assert_eq!(shown["segment"], created["segment"]);
    assert!(
        w.daemon
            .operator_rpc(
                "app_segment_show",
                json!({"install_id": install, "context_id": context_id, "segment_id": "ex-2"}),
            )
            .is_err(),
        "forged save smuggled a row"
    );
}

#[test]
fn cad780_freeze_replay_ceiling_and_consent_drift_over_rpc() {
    let w = Audiences::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-1");
    let context_id = context["id"].as_str().unwrap();
    w.seed(install, context_id);

    let prepared = w
        .daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50}),
        )
        .unwrap();
    // Identical ceiling replays; a different ceiling refuses.
    let replayed = w
        .daemon
        .operator_rpc(
            "app_audience_prepare",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 50}),
        )
        .unwrap();
    assert_eq!(replayed["freeze"]["replayed"], true);
    assert_eq!(replayed["freeze"]["digest"], prepared["freeze"]["digest"]);
    assert!(
        w.daemon
            .operator_rpc(
                "app_audience_prepare",
                json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1", "base": {"mode": "all"}, "max_recipients": 51}),
            )
            .is_err(),
        "freeze ceiling change replayed the prior ceiling"
    );
    // Consent drift invalidates the frozen approval: customer-a
    // keeps its address but loses email consent.
    w.daemon
        .operator_rpc(
            "app_record_update",
            json!({"install_id": install, "context_id": context_id, "record_id": "customer-a", "expected_revision": 1, "profile": {"schema": 1, "display_name": "Amina Diallo", "email": "amina@example.com", "tags": ["vip"], "consent": {"email": "denied"}}}),
        )
        .unwrap();
    let drifted = w
        .daemon
        .operator_rpc(
            "app_audience_show",
            json!({"install_id": install, "context_id": context_id, "freeze_id": "freeze-1"}),
        )
        .unwrap();
    assert_eq!(drifted["valid"], false);
}
