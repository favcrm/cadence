use cadence_agent::contract_fixture::{classify_call, Effect};
use cadence_agent::daemon::ServeOptions;
use cadence_agent::platform::{agenticos, deployments::DeploymentMetadata};
use serde_json::json;

#[test]
fn cad677_actual_attach_ignores_caller_environment_pin() {
    if let Ok(metadata) = std::env::var("CAD677_FIXTURE_METADATA") {
        let mut opts = ServeOptions {
            provider_deployments: Some(DeploymentMetadata::parse(metadata.as_bytes()).unwrap()),
            ..Default::default()
        };
        let hosted = cadence_agent::lease::Hosted {
            lease: (std::env::var("CAD677_FIXTURE_HOSTED").unwrap() == "true")
                .then(|| "file:/unused-fixture-lease".into()),
            ..Default::default()
        };
        agenticos::attach(&mut opts, &hosted).unwrap();
        let adapter = &opts.platforms["agenticos"];
        let expected = if std::env::var("CAD677_FIXTURE_DRAFT").unwrap() == "true" {
            Effect::Draft
        } else {
            Effect::Send
        };
        assert_eq!(
            classify_call(
                adapter.table(),
                adapter.reported_manifest_version().as_deref(),
                "publish_post"
            ),
            expected
        );
        return;
    }
    for (origin, pin, hosted, draft) in [
        (
            "http://api.internal",
            "agenticos-manifest@1/publish_post@2",
            true,
            true,
        ),
        ("http://api.internal", "old-contract", true, false),
        (
            "http://other.internal",
            "agenticos-manifest@1/publish_post@2",
            true,
            false,
        ),
        (
            "http://api.internal",
            "agenticos-manifest@1/publish_post@2",
            false,
            false,
        ),
        ("http://api.internal", "", true, false),
    ] {
        let providers = if pin.is_empty() {
            json!([])
        } else {
            json!([{"provider":"agenticos","origin":origin,"manifest_pin":pin}])
        };
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "cad677_actual_attach_ignores_caller_environment_pin",
                "--nocapture",
            ])
            .env(
                "CAD677_FIXTURE_METADATA",
                json!({"schema":1,"providers":providers}).to_string(),
            )
            .env("CAD677_FIXTURE_HOSTED", hosted.to_string())
            .env("CAD677_FIXTURE_DRAFT", draft.to_string())
            .env("CADENCE_AGENTICOS_URL", "http://api.internal")
            .env(
                "CADENCE_AGENTICOS_DEPLOYMENT_PIN",
                "agenticos-manifest@1/publish_post@2",
            )
            .env(
                "AGENTICOS_DEPLOYMENT_PIN",
                "agenticos-manifest@1/publish_post@2",
            )
            .env("AGENTICOS_BOARD_COMPANY", "forged-company");
        assert!(cadence_agent::reaper::status(&mut command)
            .unwrap()
            .success());
    }
}

#[test]
fn hosted_composition_requires_exact_provider_origin_and_manifest_pin() {
    for (origin, pin, hosted, effect) in [
        (
            "http://api.internal",
            "agenticos-manifest@1/publish_post@2",
            true,
            Effect::Draft,
        ),
        ("http://api.internal", "old-contract", true, Effect::Send),
        (
            "http://company.internal",
            "agenticos-manifest@1/publish_post@2",
            true,
            Effect::Send,
        ),
        (
            "http://api.internal",
            "agenticos-manifest@1/publish_post@2",
            false,
            Effect::Send,
        ),
    ] {
        let metadata = DeploymentMetadata::parse(
            &serde_json::to_vec(&json!({
                "schema":1,"providers":[{"provider":"agenticos","origin":origin,"manifest_pin":pin}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut opts = ServeOptions {
            provider_deployments: Some(metadata),
            ..Default::default()
        };
        agenticos::register_from_composition(&mut opts, "http://api.internal", hosted).unwrap();
        let adapter = &opts.platforms["agenticos"];
        assert_eq!(
            classify_call(
                adapter.table(),
                adapter.reported_manifest_version().as_deref(),
                "publish_post"
            ),
            effect
        );
    }
    let mut opts = ServeOptions {
        provider_deployments: Some(
            DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap(),
        ),
        ..Default::default()
    };
    agenticos::register_from_composition(&mut opts, "http://api.internal", true).unwrap();
    assert_eq!(
        opts.platforms["agenticos"].reported_manifest_version(),
        None
    );
}
