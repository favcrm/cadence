//! CAD-782 versioned email content over the real daemon socket.
//!
//! Adversarial-first at the daemon: an operator caller gets typed
//! content save/show/list/render, proposal propose/apply/discard,
//! approval and test/final-send preparation with revision CAS and
//! frozen digests; an agent caller, a detached child, forged
//! `by`/`actor`/`project`/`project_link`/`workspace` fields,
//! cross-install and cross-context probes (reads included — every
//! action proves the live context), and stale/concurrent writes are
//! all refused without mutation. No SMTP send happens anywhere.
#![allow(clippy::disallowed_methods)]
mod common;
use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, LaneShell, TestDaemon};
use serde_json::{json, Value};
use std::path::PathBuf;

fn blocks() -> Value {
    json!([
        {"type": "heading", "text": "Hello {{first_name|friend}}"},
        {"type": "paragraph", "text": "A calm first line."},
        {"type": "button", "label": "Read more", "url": "https://example.com/posts/welcome"},
    ])
}

struct Content {
    _root: tempfile::TempDir,
    _pm: Pm,
    daemon: TestDaemon,
}

impl Content {
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

    fn save(&self, install: &str, context: &str, campaign: &str, rev: Option<u64>) -> Value {
        let mut params = json!({"install_id": install, "context_id": context, "campaign_id": campaign, "subject": "Spring launch", "preheader": "News", "blocks": blocks()});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        self.daemon
            .operator_rpc("app_content_save", params)
            .unwrap()
    }

    fn show(&self, install: &str, context: &str, campaign: &str) -> Value {
        self.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context, "campaign_id": campaign}),
            )
            .unwrap()
    }
}

#[test]
fn cad782_operator_content_roundtrip_with_proposal_and_approval() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-1");
    let context_id = context["id"].as_str().unwrap();

    let created = w.save(install, context_id, "launch-1", None);
    assert_eq!(created["content"]["revision"], 1);
    assert!(created["content"]["content_digest"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"],
        created["content"]
    );
    let listed = w
        .daemon
        .operator_rpc(
            "app_content_list",
            json!({"install_id": install, "context_id": context_id}),
        )
        .unwrap();
    assert_eq!(listed["contents"].as_array().unwrap().len(), 1);

    // Render derives from the same revision in both shapes.
    let rendered = w
        .daemon
        .operator_rpc(
            "app_content_render",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "sample_first_name": "Amina"}),
        )
        .unwrap();
    assert_eq!(rendered["render"]["revision"], 1);
    assert_eq!(
        rendered["render"]["content_digest"],
        created["content"]["content_digest"]
    );
    assert!(rendered["render"]["html"]
        .as_str()
        .unwrap()
        .contains("Hello Amina"));
    assert!(rendered["render"]["text"]
        .as_str()
        .unwrap()
        .contains("Hello Amina"));
    assert!(rendered["render"]["html"]
        .as_str()
        .unwrap()
        .contains("noreply@cadence.invalid"));

    // Proposal is inert until Apply; Discard changes nothing.
    let proposed = w
        .daemon
        .operator_rpc(
            "app_content_propose",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "proposal_id": "prop-1", "subject": "Spring launch, new", "preheader": "News", "blocks": blocks()}),
        )
        .unwrap();
    assert_eq!(proposed["proposal"]["state"], "pending");
    assert_eq!(proposed["proposal"]["actor"], "assistant");
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"],
        created["content"]
    );
    w.daemon
        .operator_rpc(
            "app_content_proposal_discard",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1"}),
        )
        .unwrap();
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"],
        created["content"]
    );

    // Apply creates a new revision; approval pins it; test and final
    // preparation share the content hash with locked sender material.
    w.daemon
        .operator_rpc(
            "app_content_propose",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "proposal_id": "prop-2", "subject": "Spring launch, applied", "preheader": "News", "blocks": blocks()}),
        )
        .unwrap();
    let applied = w
        .daemon
        .operator_rpc(
            "app_content_proposal_apply",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-2", "expected_revision": 1}),
        )
        .unwrap();
    assert_eq!(applied["content"]["revision"], 2);
    let approved = w
        .daemon
        .operator_rpc(
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "expected_revision": 2}),
        )
        .unwrap();
    assert_eq!(approved["content"]["approval"]["valid"], true);
    let test = w
        .daemon
        .operator_rpc(
            "app_content_test_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "to_email": "op@example.com"}),
        )
        .unwrap();
    let send = w
        .daemon
        .operator_rpc(
            "app_content_send_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        )
        .unwrap();
    assert_eq!(
        test["test_send"]["content_digest"],
        send["send"]["content_digest"]
    );
    assert_eq!(test["test_send"]["html"], send["send"]["html"]);
    assert_eq!(test["test_send"]["text"], send["send"]["text"]);
}

#[test]
fn cad782_reads_prove_the_active_context() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-2");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);

    // Unknown contexts refuse on every read path.
    for (method, params) in [
        (
            "app_content_show",
            json!({"install_id": install, "context_id": "ctx-no-such-context", "campaign_id": "launch-1"}),
        ),
        (
            "app_content_list",
            json!({"install_id": install, "context_id": "ctx-no-such-context"}),
        ),
        (
            "app_content_render",
            json!({"install_id": install, "context_id": "ctx-no-such-context", "campaign_id": "launch-1"}),
        ),
        (
            "app_content_proposal_show",
            json!({"install_id": install, "context_id": "ctx-no-such-context", "proposal_id": "prop-1"}),
        ),
        (
            "app_content_proposal_list",
            json!({"install_id": install, "context_id": "ctx-no-such-context"}),
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
            "app_content_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_content_render",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "archived-context read admitted by {method}"
        );
    }
}

#[test]
fn cad782_cross_install_probes_refuse() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-3");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);
    let second = w.install_second();
    let install_b = second["install_id"].as_str().unwrap();
    let context_b = w.context(install_b, "Client", "ctx-content-3b");
    let context_b_id = context_b["id"].as_str().unwrap();

    // The sibling installation cannot read or write the first one's
    // content, and unknown installations refuse before any file opens.
    for (method, params) in [
        (
            "app_content_show",
            json!({"install_id": install_b, "context_id": context_id, "campaign_id": "launch-1"}),
        ),
        (
            "app_content_list",
            json!({"install_id": install_b, "context_id": context_id}),
        ),
        (
            "app_content_save",
            json!({"install_id": install_b, "context_id": context_id, "campaign_id": "launch-1", "subject": "Hijack", "blocks": blocks()}),
        ),
        (
            "app_content_show",
            json!({"install_id": install, "context_id": context_b_id, "campaign_id": "launch-1"}),
        ),
        (
            "app_content_show",
            json!({"install_id": "no-such-install", "context_id": context_id, "campaign_id": "launch-1"}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "cross-install probe admitted by {method}"
        );
    }
    // The first installation's content is untouched.
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"]["revision"],
        1
    );
}

#[test]
fn cad782_stale_and_concurrent_edits_refuse_without_mutation() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-4");
    let context_id = context["id"].as_str().unwrap();
    let created = w.save(install, context_id, "launch-1", None);

    // Blind and stale overwrites refuse and change nothing.
    for rev in [None, Some(7)] {
        let mut params = json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "subject": "Racer", "blocks": blocks()});
        if let Some(expected) = rev {
            params["expected_revision"] = json!(expected);
        }
        assert!(
            w.daemon.operator_rpc("app_content_save", params).is_err(),
            "stale content save accepted"
        );
    }
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"],
        created["content"]
    );

    // Concurrent saves at the same observed revision: exactly one wins.
    let attempts = 6;
    let results = std::thread::scope(|scope| {
        (0..attempts)
            .map(|_| {
                scope.spawn(|| {
                    w.daemon
                        .operator_rpc(
                            "app_content_save",
                            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "subject": "Racer", "blocks": blocks(), "expected_revision": 1}),
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
        "concurrent content CAS admitted {results:?}"
    );
    assert_eq!(
        w.show(install, context_id, "launch-1")["content"]["revision"],
        2
    );
}

#[test]
fn cad782_agent_forged_and_detached_callers_cannot_touch_content() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-5");
    let context_id = context["id"].as_str().unwrap();
    w.save(install, context_id, "launch-1", None);
    let before = w.show(install, context_id, "launch-1");

    let mut lane = LaneShell::spawn(w._root.path());
    plant_member_pane(&w.daemon, "content-worker", "claude", None, lane.pid());
    let calls: Vec<(&str, Value)> = vec![
        (
            "app_content_save",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-evil", "subject": "Evil", "blocks": blocks()}),
        ),
        (
            "app_content_show",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        ),
        (
            "app_content_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_content_render",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
        ),
        (
            "app_content_propose",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "proposal_id": "prop-evil", "subject": "Evil", "blocks": blocks()}),
        ),
        (
            "app_content_proposal_show",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1"}),
        ),
        (
            "app_content_proposal_list",
            json!({"install_id": install, "context_id": context_id}),
        ),
        (
            "app_content_proposal_apply",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1"}),
        ),
        (
            "app_content_proposal_discard",
            json!({"install_id": install, "context_id": context_id, "proposal_id": "prop-1"}),
        ),
        (
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "expected_revision": 1}),
        ),
        (
            "app_content_test_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "to_email": "evil@example.com"}),
        ),
        (
            "app_content_send_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
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
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "by": "operator", "actor": "operator"}),
        json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "project": "client", "project_link": "client", "workspace": "default"}),
    ] {
        let frame = lane.rpc(&w.daemon.state, "app_content_show", params);
        assert_eq!(frame["ok"], false, "forged fields reached content: {frame}");
    }
    // A detached child of the agent — no provable identity — is refused too.
    let request = lane.dir.path().join("detached.json");
    std::fs::write(
        &request,
        cadence_agent::proto::request(
            "app_content_render",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1"}),
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
    assert_eq!(w.show(install, context_id, "launch-1"), before);
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_show",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-evil"}),
            )
            .is_err(),
        "agent smuggled content"
    );
}

#[test]
fn cad782_operator_forged_fields_refuse_with_valid_control() {
    let w = Content::new();
    let installed = w.install();
    let install = installed["install_id"].as_str().unwrap();
    let context = w.context(install, "Client", "ctx-content-6");
    let context_id = context["id"].as_str().unwrap();

    // Valid control: the exact same shape without forged fields works.
    let created = w.save(install, context_id, "launch-1", None);
    assert_eq!(created["content"]["revision"], 1);

    // Forged identity fields refuse for the operator caller as well.
    for (method, params) in [
        (
            "app_content_show",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "by": "operator"}),
        ),
        (
            "app_content_list",
            json!({"install_id": install, "context_id": context_id, "actor": "operator"}),
        ),
        (
            "app_content_render",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "by": "operator"}),
        ),
        (
            "app_content_propose",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "proposal_id": "prop-1", "subject": "S", "blocks": blocks(), "actor": "assistant"}),
        ),
        (
            "app_content_approve",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "expected_revision": 1, "actor": "operator"}),
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
            "app_content_show",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "project": "client"}),
        ),
        (
            "app_content_list",
            json!({"install_id": install, "context_id": context_id, "project_link": "client"}),
        ),
        (
            "app_content_save",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "subject": "S", "blocks": blocks(), "workspace": "default"}),
        ),
        (
            "app_content_test_prepare",
            json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-1", "to_email": "op@example.com", "project": "client"}),
        ),
    ] {
        assert!(
            w.daemon.operator_rpc(method, params).is_err(),
            "routing/discovery field accepted by {method}"
        );
    }
    // Unsafe content refuses through the daemon grammar too.
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-2", "subject": "<b>Hi</b>", "blocks": blocks()}),
            )
            .is_err(),
        "HTML subject accepted"
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-2", "subject": "Hi", "blocks": [{"type": "button", "label": "Go", "url": "javascript:alert(1)"}]}),
            )
            .is_err(),
        "unsafe button URL accepted"
    );
    assert!(
        w.daemon
            .operator_rpc(
                "app_content_save",
                json!({"install_id": install, "context_id": context_id, "campaign_id": "launch-2", "subject": "Hi", "blocks": [{"type": "paragraph", "text": "Hi {{last_name|x}}"}]}),
            )
            .is_err(),
        "unapproved token accepted"
    );
}
