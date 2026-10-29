//! CAD-1942 reviewer-owned acceptance, authored by cc13-rev-sol from the ticket.
//!
//! Integration: copy this file to `src/ui/cad1942_by_reference_refusal.rs` and
//! append these declarations at module scope in `src/ui/apps.rs`:
//! ```ignore
//! #[cfg(test)]
//! #[path = "cad1942_by_reference_refusal.rs"]
//! mod cad1942_by_reference_refusal;
//! ```
//! Run through `scripts/boat-build -- cargo test --locked --lib
//! cad1942_by_reference_refusal -- --test-threads 1 --nocapture`.
//!
//! One check: actual HTTP handlers reject bad requests before RPC; actual
//! fetch/digest/staging code processes the loopback catalogue. Positive install,
//! upgrade and stale-pin checks call the real workspace backend, not a fake
//! daemon returning canned refusals. This is not an operator-auth, browser-QA
//! or running-daemon test. Only the catalogue is a transport double.

use super::*;
use crate::issue::app_catalog::{
    catalogue::{claim_from_object, CatalogueDirectory},
    workspace as native,
};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const APP: &str = "cad1942-synthetic";
const CHILD: &str = "CAD1942_REFUSAL_ACCEPTANCE_CHILD";
type Reply = Option<(u16, String)>;

#[derive(Clone, Copy, Debug)]
enum Route {
    InstallCheck,
    Install,
    UpgradeCheck,
    Upgrade,
}

impl Route {
    fn upgrade(self) -> bool {
        matches!(self, Self::UpgradeCheck | Self::Upgrade)
    }

    fn check(self) -> bool {
        matches!(self, Self::InstallCheck | Self::UpgradeCheck)
    }

    fn method(self) -> &'static str {
        if self.check() {
            "app_workspace_upgrade_check"
        } else {
            "app_workspace_upgrade"
        }
    }

    fn body(self, locator: &Value, files: Option<&BTreeMap<String, String>>) -> Value {
        let mut body = json!({"catalogue": locator,
            "expected_digest": if self.upgrade() { json!(other_digest()) } else { locator["digest"].clone() }});
        if self.upgrade() {
            body["expected_generation"] = json!("synthetic-old-generation");
            if !self.check() {
                body["expected_new_digest"] = locator["digest"].clone();
                body["request_id"] = json!("cad1942-refusal");
            }
        }
        if let Some(files) = files {
            body["files"] = json!(files);
        }
        body
    }

    fn call(self, raw: &str, state: &Path) -> HttpResp {
        let path = match self {
            Self::InstallCheck => "/api/app-installations/upload/check",
            Self::Install => "/api/app-installations/upload",
            Self::UpgradeCheck => "/api/app-installations/cad1942-install/upgrade/check",
            Self::Upgrade => "/api/app-installations/cad1942-install/upgrade",
        };
        let mut request: Request = tiny_http::TestRequest::new()
            .with_method(tiny_http::Method::Post)
            .with_path(path)
            // tiny_http 0.12 requires a static test body. This isolated child
            // makes finitely many requests; its owned copies die at exit.
            .with_body(Box::leak(raw.to_owned().into_boxed_str()))
            .into();
        if self.upgrade() {
            workspace(&mut request, state, self.method(), Some("cad1942-install"))
        } else if self.check() {
            workspace_upload_check(&mut request, state)
        } else {
            workspace_upload(&mut request, state)
        }
    }
}

fn other_digest() -> String {
    format!("sha256:{}", "ab".repeat(32))
}

fn bundle(version: &str) -> BTreeMap<String, String> {
    let mut files = BTreeMap::from([
        ("app.md".into(), format!("---\napp: {APP}\ntitle: Synthetic acceptance\nversion: '{version}'\nneeds:\n  connections: []\n---\n\nSynthetic guide.\n")),
        ("workflows/run.md".into(), "---\ntitle: Synthetic work\ngoal: Complete synthetic work\n---\n\nWhy.\n\n## Work\nagent: acceptance-worker\nsize: S\n\nDo synthetic work.\n\n### Acceptance\n- [ ] synthetic work complete\n".into()),
    ]);
    // Over 64 KiB in total without increasing any individual file cap.
    for name in ["one", "two", "three"] {
        files.insert(
            format!("templates/{name}.md"),
            format!("{version}\n{}", "synthetic text\n".repeat(1800)),
        );
    }
    assert!(files.values().map(String::len).sum::<usize>() > 64 * 1024);
    files
}

fn locator(version: &str, files: &BTreeMap<String, String>) -> Value {
    json!({"id": APP, "version": version, "digest": native::bundle_digest(files)})
}

struct Catalogue {
    origin: String,
    hits: Arc<Mutex<Vec<String>>>,
    reply: Arc<Mutex<Reply>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Catalogue {
    fn start(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Self {
        let server = (3110..=3199)
            .find_map(|port| tiny_http::Server::http(("127.0.0.1", port)).ok())
            .expect("a private loopback catalogue fixture port");
        let origin = format!("http://{}", server.server_addr().to_ip().unwrap());
        let hits = Arc::new(Mutex::new(Vec::new()));
        let reply = Arc::new(Mutex::new(None::<(u16, String)>));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&hits);
        let override_reply = Arc::clone(&reply);
        let stopped = Arc::clone(&stop);
        let versions = json!({"app_id": APP, "app": APP, "recommended_version": "2.0.0",
            "releases": [
                {"version":"1.0.0", "digest":native::bundle_digest(old), "installable":true, "unavailableReason":null, "release_notes":"synthetic"},
                {"version":"2.0.0", "digest":native::bundle_digest(new), "installable":true, "unavailableReason":null, "release_notes":"synthetic"}
            ]}).to_string();
        let releases: BTreeMap<String, String> = [("1.0.0", old), ("2.0.0", new)]
            .into_iter()
            .map(|(version, files)| {
                let digest = native::bundle_digest(files);
                (
                    format!(
                        "/public/apps/{APP}/release?version={version}&digest={}",
                        digest.replace(':', "%3A")
                    ),
                    json!({"files":files,"expected_digest":digest}).to_string(),
                )
            })
            .collect();
        let thread = std::thread::spawn(move || {
            while !stopped.load(SeqCst) {
                let Some(request) = server.recv_timeout(Duration::from_millis(30)).unwrap() else {
                    continue;
                };
                seen.lock().unwrap().push(request.url().to_string());
                assert_eq!(request.method(), &tiny_http::Method::Get);
                assert!(!request
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("Cookie") || h.field.equiv("Authorization")));
                let (status, text) = if request.url() == format!("/public/apps/{APP}/versions") {
                    (200, versions.clone())
                } else if let Some(document) = releases.get(request.url()) {
                    override_reply
                        .lock()
                        .unwrap()
                        .clone()
                        .unwrap_or((200, document.clone()))
                } else {
                    (404, "{\"error\":\"unknown synthetic release\"}".into())
                };
                request
                    .respond(tiny_http::Response::from_string(text).with_status_code(status))
                    .unwrap();
            }
        });
        Self {
            origin,
            hits,
            reply,
            stop,
            thread: Some(thread),
        }
    }

    fn count(&self) -> usize {
        self.hits.lock().unwrap().len()
    }
}

impl Drop for Catalogue {
    fn drop(&mut self) {
        self.stop.store(true, SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn refuse(
    route: Route,
    raw: &str,
    state: &Path,
    catalogue: &Catalogue,
    statuses: &[u16],
    fetches: usize,
    needle: &str,
) {
    let before = catalogue.count();
    let response = route.call(raw, state);
    let status = response.status_code().0;
    let mut text = String::new();
    response.into_reader().read_to_string(&mut text).unwrap();
    assert!(
        statuses.contains(&status),
        "{route:?}: expected {statuses:?}, got {status}: {text}; request {raw}"
    );
    assert!(
        text.contains(needle),
        "{route:?}: missing {needle:?} in {text}"
    );
    assert_eq!(
        catalogue.count() - before,
        fetches,
        "{route:?}: unexpected upstream request for {raw}"
    );
    // No daemon is running. A fall-through RPC cannot masquerade as the 503
    // configuration refusal, whose actual message is asserted separately.
    assert!(std::fs::read_dir(state).unwrap().next().is_none());
}

fn appended(body: &Value, field: &str) -> String {
    let raw = body.to_string();
    format!("{},{}{}", &raw[..raw.len() - 1], field, "}")
}

fn staged_upgrade(route: Route, id: &str, body: Value) -> (Value, tempfile::TempDir) {
    assert!(body.get("files").is_none());
    assert!(body.to_string().len() < 1024);
    let wire: UpgradeWire = serde_json::from_value(body).unwrap();
    match workspace_upgrade_transport(route.method(), id, wire, &configured_release_files) {
        Ok((params, Some(staging))) => {
            assert_eq!(params["install_id"], id);
            (params, staging)
        }
        Ok(_) => panic!("reference upgrade must retain its staged package"),
        Err(response) => panic!("reference upgrade refused: {}", response.status_code().0),
    }
}

#[test]
fn cad1942_by_reference_refusal() {
    // Re-exec only this test before changing catalogue configuration. Other
    // tests' environment and the managed caller's identity remain untouched.
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        for name in ["home", "tmp", "config", "cache", "state"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let full_name = format!("{}::cad1942_by_reference_refusal", module_path!());
        let test_name = full_name.split_once("::").unwrap().1;
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads", "1"])
            .env(CHILD, "1")
            .env("HOME", root.path().join("home"))
            .env("TMPDIR", root.path().join("tmp"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_STATE_HOME", root.path().join("state"))
            .env("CADENCE_STATE_DIR", root.path().join("state"))
            .env("CADENCE_SOCKET", root.path().join("state/never-bound.sock"))
            .env("AGENTICOS_BOARD_ISSUER", "https://issuer.example.test")
            .env(
                "AGENTICOS_BOARD_CATALOG_ORIGIN",
                "http://not-a-loopback-catalogue.invalid",
            )
            .env("GIT_AUTHOR_NAME", "Synthetic acceptance")
            .env("GIT_AUTHOR_EMAIL", "acceptance@example.test")
            .env("GIT_COMMITTER_NAME", "Synthetic acceptance")
            .env("GIT_COMMITTER_EMAIL", "acceptance@example.test")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "acceptance child failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("CAD1942 REAL GUARDS COMPLETE"),
            "the exact child test must actually run"
        );
        return;
    }

    let old = bundle("1.0.0");
    let new = bundle("2.0.0");
    let selected = locator("2.0.0", &new);
    let catalogue = Catalogue::start(&old, &new);
    let state = tempfile::tempdir().unwrap();
    let routes = [
        Route::InstallCheck,
        Route::Install,
        Route::UpgradeCheck,
        Route::Upgrade,
    ];

    // An invalid explicit configuration must not silently use the public
    // production default. Absence of an override is NOT "no catalogue".
    for route in routes {
        refuse(
            route,
            &route.body(&selected, None).to_string(),
            state.path(),
            &catalogue,
            &[503],
            0,
            "public directory is not configured",
        );
    }
    std::env::set_var("AGENTICOS_BOARD_CATALOG_ORIGIN", &catalogue.origin);

    for route in routes {
        for legacy in [false, true] {
            let posted = legacy.then_some(&new);
            let good = route.body(&selected, posted);
            // All browser pins stay consistent with a malformed locator, so
            // the locator guard, not a contradictory pin, must refuse it.
            for (field, value) in [
                ("id", json!("../admin")),
                ("id", json!("a/b")),
                ("id", json!("a".repeat(65))),
                ("version", json!("2.0.0?private=1")),
                ("digest", json!("sha256:short")),
            ] {
                let mut malformed = selected.clone();
                malformed[field] = value;
                refuse(
                    route,
                    &route.body(&malformed, posted).to_string(),
                    state.path(),
                    &catalogue,
                    &[400],
                    0,
                    "catalogue locator",
                );
            }
            for malformed in [
                json!({"id":APP,"digest":selected["digest"]}),
                json!({"id":APP,"version":"2.0.0","digest":selected["digest"],"url":"https://private.example.test"}),
            ] {
                let mut bad = good.clone();
                bad["catalogue"] = malformed;
                refuse(
                    route,
                    &bad.to_string(),
                    state.path(),
                    &catalogue,
                    &[400],
                    0,
                    "",
                );
            }
            if !route.upgrade() || !route.check() {
                let mut bad = good.clone();
                bad[if route.upgrade() {
                    "expected_new_digest"
                } else {
                    "expected_digest"
                }] = json!(other_digest());
                refuse(
                    route,
                    &bad.to_string(),
                    state.path(),
                    &catalogue,
                    &[409],
                    0,
                    "does not match the catalogue locator",
                );
            }
            let mut unknown = selected.clone();
            unknown["version"] = json!("9.9.9");
            refuse(
                route,
                &route.body(&unknown, posted).to_string(),
                state.path(),
                &catalogue,
                &[404, 409],
                1,
                "catalogue release is not installable",
            );

            let mut tampered = new.clone();
            tampered
                .get_mut("templates/one.md")
                .unwrap()
                .push_str("changed bytes\n");
            *catalogue.reply.lock().unwrap() = Some((
                200,
                json!({"files":tampered,"expected_digest":selected["digest"]}).to_string(),
            ));
            // Legacy copies agree with EACH OTHER, not with the digest.
            refuse(
                route,
                &route
                    .body(&selected, legacy.then_some(&tampered))
                    .to_string(),
                state.path(),
                &catalogue,
                &[409],
                1,
                "catalogue release bytes do not match",
            );
            *catalogue.reply.lock().unwrap() = Some((
                200,
                json!({"files":new,"expected_digest":other_digest()}).to_string(),
            ));
            refuse(
                route,
                &good.to_string(),
                state.path(),
                &catalogue,
                &[409],
                1,
                "catalogue release bytes do not match",
            );
            *catalogue.reply.lock().unwrap() = Some((
                200,
                json!({"files":{},"expected_digest":selected["digest"]}).to_string(),
            ));
            refuse(
                route,
                &good.to_string(),
                state.path(),
                &catalogue,
                &[409],
                1,
                "catalogue release bytes do not match",
            );
            *catalogue.reply.lock().unwrap() = None;
        }

        let mut differing = new.clone();
        differing
            .get_mut("templates/one.md")
            .unwrap()
            .push_str("posted forgery\n");
        refuse(
            route,
            &route.body(&selected, Some(&differing)).to_string(),
            state.path(),
            &catalogue,
            &[409],
            1,
            "posted files differ",
        );
        let good = route.body(&selected, None);
        for field in [
            format!("\"catalogue\":{selected}"),
            format!("\"expected_digest\":{}", good["expected_digest"]),
            "\"files\":{\"app.md\":\"first\",\"app.md\":\"second\"}".into(),
            "\"catalogue_resolution\":\"manual\"".into(),
        ] {
            refuse(
                route,
                &appended(&good, &field),
                state.path(),
                &catalogue,
                &[400],
                0,
                "",
            );
        }
        let with_files = route.body(&selected, Some(&new));
        refuse(
            route,
            &appended(&with_files, "\"files\":{\"app.md\":\"duplicate\"}"),
            state.path(),
            &catalogue,
            &[400],
            0,
            "",
        );
        let mut empty = good;
        empty["files"] = json!({});
        refuse(
            route,
            &empty.to_string(),
            state.path(),
            &catalogue,
            &[400, 409],
            usize::from(route.upgrade()),
            "",
        );
        empty.as_object_mut().unwrap().remove("catalogue");
        refuse(
            route,
            &empty.to_string(),
            state.path(),
            &catalogue,
            &[400],
            0,
            "",
        );
    }

    // Actual backend outcomes, using the very bytes the real fetch verified.
    // No canned RPC response stands in for stale-pin or publication guards.
    let pm_root = tempfile::tempdir().unwrap();
    let pm = Pm::init(pm_root.path()).unwrap();
    let initial = Route::Install.body(&locator("1.0.0", &old), None);
    let upload: UploadBody = serde_json::from_value(initial.clone()).unwrap();
    assert!(upload.files.is_none());
    let fetched = configured_release_files(upload.catalogue.as_ref().unwrap(), upload.files)
        .unwrap_or_else(|r| panic!("reference install fetch failed: {}", r.status_code().0));
    let (_source, source) =
        stage_files(&fetched).unwrap_or_else(|_| panic!("stage verified install"));
    let claim = claim_from_object(initial.as_object().unwrap()).unwrap();
    let proposal = native::install_check(&pm, &source, &claim).unwrap();
    assert_eq!(proposal["digest"], initial["catalogue"]["digest"]);
    assert_eq!(proposal["committed"], false);
    let installed = native::install(
        &pm,
        state.path(),
        &source,
        upload.expected_digest.as_deref(),
        &claim,
    )
    .unwrap();
    assert_eq!(installed["committed"], true);
    assert_eq!(installed["catalogue"], initial["catalogue"]);
    let id = installed["install_id"].as_str().unwrap();
    let before = native::show(&pm, id).unwrap();

    let mut check_body = Route::UpgradeCheck.body(&selected, None);
    check_body["expected_digest"] = before["digest"].clone();
    check_body["expected_generation"] = before["catalog_generation"].clone();
    let fetch_before = catalogue.count();
    let (check_params, _check_stage) = staged_upgrade(Route::UpgradeCheck, id, check_body);
    assert_eq!(catalogue.count() - fetch_before, 1);
    let source = check_params["source"].as_str().unwrap();
    let next_claim = claim_from_object(check_params.as_object().unwrap()).unwrap();
    let checked = native::upgrade_check(
        &pm,
        id,
        source,
        before["digest"].as_str().unwrap(),
        before["catalog_generation"].as_str().unwrap(),
        &next_claim,
    )
    .unwrap();
    assert_eq!(checked["committed"], false);
    assert_eq!(checked["digest"], selected["digest"]);
    assert_eq!(native::show(&pm, id).unwrap(), before);

    let mut apply_body = Route::Upgrade.body(&selected, None);
    apply_body["expected_digest"] = before["digest"].clone();
    apply_body["expected_generation"] = before["catalog_generation"].clone();
    let fetch_before = catalogue.count();
    let (params, _apply_stage) = staged_upgrade(Route::Upgrade, id, apply_body);
    assert_eq!(
        catalogue.count() - fetch_before,
        1,
        "apply must fetch again"
    );
    let source = params["source"].as_str().unwrap();
    for (stale_digest, stale_generation, message) in [
        (
            other_digest(),
            before["catalog_generation"].as_str().unwrap().to_string(),
            "installation digest is stale",
        ),
        (
            before["digest"].as_str().unwrap().to_string(),
            "stale-generation".into(),
            "catalog generation is stale",
        ),
    ] {
        let error = native::upgrade_check(
            &pm,
            id,
            source,
            &stale_digest,
            &stale_generation,
            &next_claim,
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        let request = native::UpgradeRequest {
            id,
            source,
            expected_digest: &stale_digest,
            expected_generation: &stale_generation,
            expected_new_digest: selected["digest"].as_str().unwrap(),
            request_id: "cad1942-stale",
            catalogue: &next_claim,
            directory: CatalogueDirectory::Configured,
        };
        let error = native::upgrade(
            &pm,
            &request,
            |_, _| panic!("stale pins reached preflight"),
            |_| panic!("stale pins reached delivery"),
        )
        .unwrap_err();
        assert!(error.to_string().contains(message), "{error}");
        assert_eq!(native::show(&pm, id).unwrap(), before);
    }
    let request = native::UpgradeRequest {
        id,
        source,
        expected_digest: before["digest"].as_str().unwrap(),
        expected_generation: before["catalog_generation"].as_str().unwrap(),
        expected_new_digest: selected["digest"].as_str().unwrap(),
        request_id: "cad1942-positive",
        catalogue: &next_claim,
        directory: CatalogueDirectory::Configured,
    };
    let upgraded = native::upgrade(
        &pm,
        &request,
        |_, _| Ok((json!({}), BTreeMap::new())),
        |_| Ok(Vec::new()),
    )
    .unwrap();
    assert_eq!(upgraded["committed"], true);
    let after = native::show(&pm, id).unwrap();
    assert_eq!(after["install_id"], before["install_id"]);
    assert_eq!(after["catalogue"], selected);
    assert_eq!(after["version"], "2.0.0");
    assert_eq!(after["digest"], selected["digest"]);
    assert_eq!(params["expected_new_digest"], after["digest"]);
    assert_eq!(
        std::fs::read_to_string(Path::new(source).join("templates/one.md")).unwrap(),
        new["templates/one.md"]
    );
    println!("CAD1942 REAL GUARDS COMPLETE");
}
