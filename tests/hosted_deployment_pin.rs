use cadence_agent::contract_fixture::{classify_call, Effect};
use cadence_agent::daemon::ServeOptions;
use cadence_agent::platform::{agenticos, deployments::DeploymentMetadata};
use serde_json::json;

#[test]
fn hosted_composition_requires_exact_provider_origin_and_manifest_pin() {
    for (origin, pin, hosted, effect) in [
        ("http://api.internal", "agenticos-manifest@1/publish_post@2", true, Effect::Draft),
        ("http://api.internal", "old-contract", true, Effect::Send),
        ("http://company.internal", "agenticos-manifest@1/publish_post@2", true, Effect::Send),
        ("http://api.internal", "agenticos-manifest@1/publish_post@2", false, Effect::Send),
    ] {
        let metadata = DeploymentMetadata::parse(&serde_json::to_vec(&json!({
            "schema":1,"providers":[{"provider":"agenticos","origin":origin,"manifest_pin":pin}]
        })).unwrap()).unwrap();
        let mut opts = ServeOptions { provider_deployments: Some(metadata), ..Default::default() };
        agenticos::register_from_composition(&mut opts, "http://api.internal", hosted).unwrap();
        let adapter = &opts.platforms["agenticos"];
        assert_eq!(classify_call(adapter.table(), adapter.reported_manifest_version().as_deref(), "publish_post"), effect);
    }
    let mut opts = ServeOptions {
        provider_deployments: Some(DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#).unwrap()),
        ..Default::default()
    };
    agenticos::register_from_composition(&mut opts, "http://api.internal", true).unwrap();
    assert_eq!(opts.platforms["agenticos"].reported_manifest_version(), None);
}
