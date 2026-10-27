//! CAD-1301: the host must parse the destinations reply AgenticOS really
//! sends. `tests/fixtures/agenticos/device-publish-destinations.json` is
//! `devicePublishFixture.destinations` copied verbatim from
//! `packages/contracts/src/device-publish.ts` in agenticos-v2 (origin/main
//! 200bc1ad140da35a5ae8826aadd9eadcd34d9471), the document behind
//! `GET /v1/runtime/connectors/destinations`. Earlier fake doors returned a
//! bare array written to match the parser, so the host saw zero accounts.
use cadence_agent::platform::agenticos_external::media_import::{DestinationLookup, MediaResolver};
use cadence_agent::platform::agenticos_external::publish_sender::DeviceCredential;
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("fixtures/agenticos/device-publish-destinations.json");
const INSTAGRAM_ID: &str = "17841400008460056";

/// Serve `data` inside the door's `{ok:true,data}` envelope on loopback.
fn serve(data: &str) -> String {
    let body = format!(r#"{{"ok":true,"data":{data}}}"#);
    let stub = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let addr = stub.server_addr().to_string();
    std::thread::spawn(move || {
        for request in stub.incoming_requests() {
            let _ = request.respond(tiny_http::Response::from_string(body.clone()));
        }
    });
    format!("http://{addr}")
}

fn resolver(base: &str) -> MediaResolver {
    MediaResolver::new(base, DeviceCredential::new("read-cred".into())).unwrap()
}

#[test]
fn the_real_destinations_fixture_lists_and_resolves_exactly_one() {
    let resolver = resolver(&serve(FIXTURE));
    let listed = resolver.list_active("instagram").expect("read succeeds");
    assert_eq!(listed.len(), 1, "the fixture's one account is listed");
    assert_eq!(listed[0].destination_id, INSTAGRAM_ID);
    assert_eq!(listed[0].display_name, "Harbour stills");
    match resolver.resolve("instagram", INSTAGRAM_ID) {
        DestinationLookup::One(found) => assert_eq!(found.aos_connection_id, "con_harbour_ig"),
        _ => panic!("the fixture destination must resolve to exactly one connection"),
    }
}

#[test]
fn a_bare_array_or_another_version_is_drift_not_zero_accounts() {
    let row: Value = serde_json::from_str::<Value>(FIXTURE).unwrap()["destinations"][0].clone();
    for drifted in [
        json!([row]),
        json!({"version": "2", "destinations": [row]}),
        json!({"version": "1"}),
    ] {
        let resolver = resolver(&serve(&drifted.to_string()));
        assert!(resolver.list_active("instagram").is_none(), "{drifted}");
        assert!(
            matches!(
                resolver.resolve("instagram", INSTAGRAM_ID),
                DestinationLookup::Unavailable
            ),
            "{drifted}"
        );
    }
}

/// The same fixture through the hosted lease door (`api.internal`, no
/// bearer, `hosted-publish` prefix), read by the Settings account list and
/// by the destination chooser that resolves the send-path connection.
#[cfg(feature = "test-seam")]
mod hosted {
    use super::*;
    use cadence_agent::platform::deployments::DeploymentMetadata;
    use cadence_agent::test_seam::{scoped, Asserted, Seam};
    use cadence_agent::{client, daemon};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    use std::sync::Arc;
    use std::time::Duration;

    const HOSTED: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;

    /// `api.internal` behind a CONNECT proxy: the destinations route
    /// answers the verbatim fixture, every other route a door error.
    fn fake_door() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    let head = |reader: &mut BufReader<_>| -> Vec<String> {
                        let mut lines = Vec::new();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                                return lines;
                            }
                            let line = line.trim_end().to_string();
                            if line.is_empty() {
                                return lines;
                            }
                            lines.push(line);
                        }
                    };
                    if !head(&mut reader)
                        .first()
                        .is_some_and(|l| l.starts_with("CONNECT "))
                    {
                        return;
                    }
                    writer
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .unwrap();
                    let request = head(&mut reader);
                    let first = request.first().cloned().unwrap_or_default();
                    let length: usize = request
                        .iter()
                        .skip(1)
                        .find_map(|l| {
                            let (k, v) = l.split_once(':')?;
                            k.eq_ignore_ascii_case("content-length")
                                .then(|| v.trim().parse().ok())?
                        })
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    let reply = if first
                        .starts_with("GET /v1/runtime/connectors/hosted-publish/destinations ")
                    {
                        format!(r#"{{"ok":true,"data":{FIXTURE}}}"#)
                    } else {
                        r#"{"ok":false,"error":{"code":"not_found","message":"no route"}}"#.into()
                    };
                    let _ = write!(
                        writer,
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                });
            }
        });
        addr
    }

    #[test]
    fn the_hosted_door_fixture_fills_the_account_list_and_resolves_one_destination() {
        for name in [
            "CADENCE_PUBLISH_SEND_URL",
            "CADENCE_PUBLISH_SEND_CREDENTIAL_FILE",
            "CADENCE_PUBLISH_READ_URL",
            "CADENCE_PUBLISH_READ_CREDENTIAL_FILE",
            "CADENCE_AGENTICOS_EXTERNAL_URL",
            "NO_PROXY",
            "no_proxy",
        ] {
            std::env::remove_var(name);
        }
        let root = tempfile::Builder::new().prefix("c1301").tempdir().unwrap();
        cadence_agent::issue::Pm::init(&root.path().join("pm")).unwrap();
        let dir = root.path().join("s");
        std::fs::create_dir_all(&dir).unwrap();
        let door = fake_door();
        let stop = Arc::new(AtomicBool::new(false));
        let env = cadence_agent::adapter::ProviderEnv::refusing_providers();
        env.set("CADENCE_PM_DIR", root.path().join("pm").to_str().unwrap());
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
            provider_deployments: Some(DeploymentMetadata::parse(HOSTED.as_bytes()).unwrap()),
            ..Default::default()
        };
        cadence_agent::platform::local::register_at(
            &dir,
            &mut opts,
            dir.join("outbox"),
            "http://127.0.0.1:3010".into(),
        );
        // ureq reads the proxy when each client is built at startup.
        std::env::set_var("ALL_PROXY", format!("http://{door}"));
        let serve_dir = dir.clone();
        let handle = std::thread::spawn(move || daemon::serve_with(&serve_dir, opts));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while client::rpc_timeout(&dir, "health", json!({}), Duration::from_secs(2)).is_err()
            || Seam::token_at(&dir).is_none()
        {
            assert!(
                !handle.is_finished() && std::time::Instant::now() < deadline,
                "daemon down"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        std::env::remove_var("ALL_PROXY");
        let op = |method: &str, params: Value| {
            scoped(Asserted::Operator, || client::rpc(&dir, method, params))
        };

        let listed = op("social_destinations", json!({})).unwrap();
        assert_eq!(listed["unavailable"], false, "{listed}");
        let rows = listed["destinations"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{listed}");
        assert_eq!(rows[0]["destination_id"], INSTAGRAM_ID);
        assert_eq!(rows[0]["label"], "Harbour stills");

        // Choosing it runs `resolve`: exactly one publishable connection.
        let source = root.path().join("app-src");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            "---\napp: c1301\ntitle: C1301\nversion: '0.1.0'\n\
             summary: Publication-slot fixture.\nneeds:\n  connections: []\n  capabilities:\n    publication:\n      schema: 1\n      capability: text.publish\n      version: 1\n      action: publish\n      resource_kind: connection_account\n      effect: send\n---\n\n# C1301\n",
        )
        .unwrap();
        std::fs::write(
            source.join("workflows/post.md"),
            "---\ntitle: \"Post\"\ngoal: \"One post\"\npublication_slot: publication\ninputs:\n  writer: { ask: \"writer\" }\n---\n\n## Write\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nWrite one post.\n\n### Acceptance\n- [ ] post exists\n",
        )
        .unwrap();
        let installed = op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        )
        .unwrap();
        let used = op(
            "app_binding_use_destination",
            json!({"install_id": installed["install_id"], "destination_id": INSTAGRAM_ID,
                "request_id": "use-1301"}),
        );

        stop.store(true, SeqCst);
        let _ = handle.join();
        let used = used.unwrap_or_else(|e| panic!("use destination: {e}"));
        assert!(
            used.to_string().contains(INSTAGRAM_ID),
            "binding names the destination: {used}"
        );
    }
}
