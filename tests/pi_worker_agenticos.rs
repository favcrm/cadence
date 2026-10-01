//! Hosted AgenticOS workers keep the operator's credentials out of their config.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::adapter::pi::PiAdapter;
use cadence_agent::adapter::{AdapterHooks, ProviderAdapter, ProviderEnv};
use cadence_agent::store::Agent;
use serde_json::{json, Value};
use std::path::Path;

const CATALOG: &str = include_str!("fixtures/agenticos-pi-models.json");

fn open_worker(root: &Path) -> cadence_agent::Result<PiAdapter> {
    let state = root.join("state");
    let pm = root.join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(pm.join("pm.yaml"), "pi:\n  models:\n    allow: [\"agenticos/z-ai/glm-5.3-flash\"]\n    default: {worker: \"agenticos/z-ai/glm-5.3-flash\"}\n").unwrap();
    std::fs::create_dir_all(state.join("agents")).unwrap();
    let env = ProviderEnv::default();
    env.set("CADENCE_PM_DIR", pm.to_string_lossy().to_string());
    env.set(
        "PI_CODING_AGENT_DIR",
        root.join("operator").to_string_lossy().to_string(),
    );
    env.set(
        "CADENCE_PI_COMMAND",
        format!(
            "python3 {} normal",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/e2e/fake-pi.py")
                .display()
        ),
    );
    let ad = PiAdapter::new(
        AdapterHooks {
            on_event: Box::new(|_, _| {}),
            on_request: Box::new(|_| {}),
        },
        &state.join("agents/w.provider.log"),
        &env,
    );
    let agent = Agent {
        alias: "w".into(),
        provider: "pi".into(),
        endpoint_kind: "managed".into(),
        role: "worker".into(),
        team_role: None,
        cwd: root.to_string_lossy().into(),
        sandbox: "read-only".into(),
        instructions: None,
        thread_id: None,
        session_id: None,
        model: None,
        effort: None,
        pid: None,
        pid_start: None,
        endpoint: None,
        params: Some(json!({"model":"agenticos/z-ai/glm-5.3-flash","effort":"off"})),
        model_selection: None,
        quota: None,
        generation: None,
        state: "starting".into(),
        enabled: true,
        error: None,
        created: 0.0,
        updated: 0.0,
    };
    ad.open(&agent)?;
    Ok(ad)
}

fn operator(root: &Path, catalog: &Value) {
    let dir = root.join("operator");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("models.json"),
        serde_json::to_vec(catalog).unwrap(),
    )
    .unwrap();
    std::fs::write(
        dir.join("auth.json"),
        r#"{"openrouter":{"key":"synthetic-operator-secret"}}"#,
    )
    .unwrap();
}

#[test]
fn agenticos_worker_gets_only_reviewed_catalog_without_operator_auth() {
    let root = tempfile::tempdir().unwrap();
    let mut catalog: Value = serde_json::from_str(CATALOG).unwrap();
    catalog["providers"]["unrelated"] = json!({"apiKey":"synthetic-unrelated-secret"});
    operator(root.path(), &catalog);
    let ad = open_worker(root.path()).unwrap();
    ad.close();
    let config = root.path().join("state/agents/w/pi");
    assert!(
        !config.join("auth.json").exists(),
        "AgenticOS must not copy any operator login"
    );
    let actual: Value =
        serde_json::from_slice(&std::fs::read(config.join("models.json")).unwrap()).unwrap();
    assert_eq!(actual, serde_json::from_str::<Value>(CATALOG).unwrap());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(config.join("models.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn agenticos_worker_refuses_changed_endpoint_key_or_model_before_launch() {
    for field in ["baseUrl", "apiKey", "models"] {
        let root = tempfile::tempdir().unwrap();
        let mut catalog: Value = serde_json::from_str(CATALOG).unwrap();
        catalog["providers"]["agenticos"][field] = json!("synthetic-forged-value");
        operator(root.path(), &catalog);
        let result = open_worker(root.path());
        if let Ok(ad) = &result {
            ad.close();
        }
        assert!(result.is_err(), "forged {field} was admitted");
        assert!(!root.path().join("state/agents/pi-record-w.json").exists());
    }
}

#[test]
fn agenticos_worker_refuses_missing_symlinked_and_oversized_catalogs() {
    for case in ["missing", "symlink", "oversized"] {
        let root = tempfile::tempdir().unwrap();
        operator(root.path(), &serde_json::from_str(CATALOG).unwrap());
        let path = root.path().join("operator/models.json");
        match case {
            "missing" => std::fs::remove_file(&path).unwrap(),
            "symlink" => {
                let real = root.path().join("catalog.json");
                std::fs::rename(&path, &real).unwrap();
                std::os::unix::fs::symlink(real, &path).unwrap();
            }
            _ => std::fs::write(&path, vec![b' '; 65_537]).unwrap(),
        }
        let result = open_worker(root.path());
        if let Ok(ad) = &result {
            ad.close();
        }
        assert!(result.is_err(), "{case} catalog was admitted");
        assert!(!root.path().join("state/agents/pi-record-w.json").exists());
        assert!(!root.path().join("state/agents/w/pi/auth.json").exists());
    }
}

#[test]
fn agenticos_worker_refresh_replaces_a_target_symlink_without_writing_its_referent() {
    let root = tempfile::tempdir().unwrap();
    operator(root.path(), &serde_json::from_str(CATALOG).unwrap());
    let ad = open_worker(root.path()).unwrap();
    ad.close();
    let path = root.path().join("state/agents/w/pi/models.json");
    std::fs::remove_file(&path).unwrap();
    let foreign = root.path().join("foreign.json");
    std::fs::write(&foreign, b"preserve-me").unwrap();
    std::os::unix::fs::symlink(&foreign, &path).unwrap();
    let ad = open_worker(root.path()).unwrap();
    ad.close();
    assert_eq!(std::fs::read(&foreign).unwrap(), b"preserve-me");
    assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), CATALOG);
}

#[test]
fn agenticos_worker_refuses_existing_auth_without_deleting_it_or_launching() {
    let root = tempfile::tempdir().unwrap();
    operator(root.path(), &serde_json::from_str(CATALOG).unwrap());
    let config = root.path().join("state/agents/w/pi");
    std::fs::create_dir_all(&config).unwrap();
    let auth = config.join("auth.json");
    let original = b"synthetic-existing-worker-login";
    std::fs::write(&auth, original).unwrap();
    let result = open_worker(root.path());
    if let Ok(ad) = &result {
        ad.close();
    }
    assert!(result.is_err(), "a credential-bearing worker was admitted");
    assert_eq!(std::fs::read(&auth).unwrap(), original);
    assert!(!config.join("models.json").exists());
    assert!(!root.path().join("state/agents/pi-record-w.json").exists());
}
