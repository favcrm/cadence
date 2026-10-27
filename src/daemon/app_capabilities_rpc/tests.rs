//! CAD-868 actual discovery, binding receipt and broker credential consumers.
use super::*;
use crate::platform::{agenticos_external, deployments::DeploymentMetadata, Custody, Key};
use std::collections::BTreeMap;

fn fixture() -> (
    tempfile::TempDir,
    Arc<Shared>,
    Value,
    BTreeMap<String, String>,
    BindingProof,
) {
    let dir = tempfile::tempdir().unwrap();
    let metadata = DeploymentMetadata::parse(br#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#).unwrap();
    let mut opts = ServeOptions::default();
    opts.platforms.insert(
        agenticos_external::PLATFORM.into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::hosted_media(
                metadata.hosted_media().unwrap(),
            )
            .unwrap(),
        ),
    );
    let mut shared = Shared::new(dir.path(), &opts).unwrap();
    // Synthetic file custody only; never consult an installed host keychain.
    Arc::get_mut(&mut shared).unwrap().platform_custody =
        Custody::file(dir.path().join("fixture-custody"));
    let connections = shared.connection_list_locked().unwrap();
    let connection = connections
        .iter()
        .find(|row| row["provider"] == "agenticos_external")
        .unwrap();
    assert_eq!(connection["kind"], "builtin");
    assert_eq!(connection["account"], "hosted");
    assert_eq!(connection["status"]["manifest_status"], "matched");
    assert_eq!(connection["status"]["custody_available"], true);
    assert_eq!(connection["status"]["network_checked"], false);
    let bundle = json!({"digest":"sha256:fixture-bundle"});
    let files = BTreeMap::from([(
        "app.md".into(),
        include_str!("../../../tests/fixtures/apps/social-content/app.md").into(),
    )]);
    let config = shared
        .app_binding_config(
            "install868",
            None,
            "image",
            connection["id"].as_str().unwrap(),
            &bundle,
            &files,
        )
        .unwrap();
    shared
        .store
        .app_binding_create("install868", None, "image", &config, "bind868")
        .unwrap();
    let proof = shared
        .store
        .app_binding_for_slot(
            "install868",
            None,
            "image",
            bundle["digest"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    (dir, shared, bundle, files, proof)
}

#[test]
fn cad868_builtin_discovery_binding_and_app_credential_are_registration_dependent() {
    let (_dir, shared, bundle, files, proof) = fixture();
    shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .unwrap();
    assert!(shared
        .app_capability_credential(&proof.config)
        .unwrap()
        .is_empty());
    assert!(shared.store.platform_credentials().unwrap().is_empty());
    // Neither discovery nor app admission silently enrolls bytes or opens a
    // credentialless legacy grant/default route.
    assert!(!crate::platform::is_builtin("agenticos_external", "hosted"));
    assert!(crate::platform::load_credential(
        &shared.store,
        &shared.platform_custody,
        "agenticos_external",
        "hosted"
    )
    .is_err());
    let connection = proof.config["connection_id"].as_str().unwrap();
    // CAD-1060: the same builtin also binds the source read, credentialless.
    let source = shared
        .app_binding_config("install868", None, "source", connection, &bundle, &files)
        .unwrap();
    assert_eq!(source["account"], "hosted");
    assert_eq!(source["connection_kind"], "builtin");
    assert_eq!(source["mapping"]["capability"], "social.read");
    assert_eq!(source["mapping"]["tool"], "read_instagram_posts");
    assert_eq!(source["mapping"]["effect"], "read");
    assert!(shared
        .app_capability_credential(&source)
        .unwrap()
        .is_empty());
    assert!(shared
        .app_binding_config(
            "install868",
            None,
            "publication",
            connection,
            &bundle,
            &files
        )
        .is_err());
}

#[test]
fn cad868_broker_quote_refuses_forged_missing_and_stale_builtin_receipts() {
    let (_dir, shared, _bundle, _files, proof) = fixture();
    for (field, value) in [
        ("schema", json!(2)),
        ("workspace_id", json!("other-workspace")),
        ("provider", json!("unregistered")),
        ("account", json!("other")),
        ("connection_kind", json!("enrolled")),
        ("connection_kind", Value::Null),
        ("connection_id", json!("missing")),
        ("connection_id", Value::Null),
        ("connection_revision", json!(1)),
        ("registration_digest", json!("sha256:forged")),
        ("registration_digest", Value::Null),
        ("sink_registration", json!("sha256:forged")),
        ("sink_registration", Value::Null),
        ("descriptor_revision", json!("external-mode")),
        ("reviewed_pin", Value::Null),
        ("reported_pin", json!("stale")),
    ] {
        let mut forged = proof.clone();
        forged.config[field] = value;
        assert!(
            shared.app_capability_credential(&forged.config).is_err(),
            "{field}"
        );
        // The real broker quote consumer must reject before media price I/O.
        assert!(shared.app_capability_quote(&forged).is_err(), "{field}");
    }
}

#[test]
fn cad868_adapter_disappearance_or_mode_switch_stales_existing_binding() {
    let (_dir, mut shared, bundle, files, proof) = fixture();
    Arc::get_mut(&mut shared)
        .unwrap()
        .platforms
        .remove("agenticos_external");
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared.app_capability_quote(&proof).is_err());
    Arc::get_mut(&mut shared).unwrap().platforms.insert(
        "agenticos_external".into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                "https://external.example.test",
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    assert!(!shared
        .connection_list_locked()
        .unwrap()
        .iter()
        .any(|row| row["provider"] == "agenticos_external"));
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared.app_capability_quote(&proof).is_err());
}

#[test]
fn cad868_legacy_external_account_named_hosted_never_upgrades_to_builtin() {
    let (dir, mut shared, bundle, files, proof) = fixture();
    let token = b"synthetic-cad868-external-token";
    let custody = shared
        .platform_custody
        .put(
            &Key {
                platform: "agenticos_external",
                account: "hosted",
            },
            token,
        )
        .unwrap();
    let record = crate::store::CredentialRecord {
        connection_id: "enrolled868".into(),
        credential_revision: 1,
        platform: "agenticos_external".into(),
        account: "hosted".into(),
        scopes: vec!["provider.draft".into()],
        fingerprint: crate::secret::fingerprint(token),
        custody: custody.into(),
        exchange: "token".into(),
        enrolled_at: 1.0,
        by: "operator".into(),
    };
    // Current enrollment rightly rejects noncanonical workspace accounts.
    let error = shared
        .store
        .platform_enroll(&record, false, None)
        .unwrap_err();
    assert!(
        error.to_string().contains("canonical workspace ID"),
        "{error}"
    );
    assert!(shared
        .store
        .platform_credential("agenticos_external", "hosted")
        .unwrap()
        .is_none());
    // Defensive legacy/corrupt-row case only: credential_row/load_credential
    // read historical records without re-running current enrollment parsing.
    // Seed just this synthetic temp database using the current explicit schema;
    // neither the store validator nor any custody/runtime guard is weakened.
    let db = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO platform_credentials (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision) VALUES(?,?,?,?,?,?,?,?,?,?)",
        rusqlite::params![record.platform, record.account, serde_json::to_string(&record.scopes).unwrap(), record.fingerprint, record.custody, record.exchange, record.enrolled_at, record.by, record.connection_id, record.credential_revision],
    ).unwrap();
    drop(db);
    let mut enrolled = proof.clone();
    enrolled.config["connection_kind"] = json!("enrolled");
    enrolled.config["connection_id"] = json!("enrolled868");
    enrolled.config["connection_revision"] = json!(1);
    assert!(shared.app_capability_credential(&enrolled.config).is_err());
    assert!(shared.app_capability_quote(&enrolled).is_err());
    // The separate builtin still holds no bytes; the enrolled custody is not
    // overwritten, reclassified or read as a fake lease credential.
    assert!(shared
        .app_capability_credential(&proof.config)
        .unwrap()
        .is_empty());
    assert_eq!(
        crate::platform::load_credential(
            &shared.store,
            &shared.platform_custody,
            "agenticos_external",
            "hosted"
        )
        .unwrap(),
        token
    );
    Arc::get_mut(&mut shared).unwrap().platforms.insert(
        "agenticos_external".into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                "https://external.example.test",
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    assert_eq!(
        shared.app_capability_credential(&enrolled.config).unwrap(),
        token
    );
    assert!(shared.app_capability_credential(&proof.config).is_err());
    assert!(shared
        .app_binding_receipt_current("install868", None, "image", &proof, &bundle, &files)
        .is_err());
}

#[test]
fn cad868_canonical_external_account_requires_enrolled_nonempty_custody() {
    const ACCOUNT: &str = "ws_11111111-1111-4111-8111-111111111111";
    let (_dir, mut shared, bundle, files, mut host_proof) = fixture();
    let token = b"synthetic-cad868-canonical-token";
    let key = Key {
        platform: "agenticos_external",
        account: ACCOUNT,
    };
    let custody = shared.platform_custody.put(&key, token).unwrap();
    shared
        .store
        .platform_enroll(
            &crate::store::CredentialRecord {
                connection_id: "canonical868".into(),
                credential_revision: 1,
                platform: "agenticos_external".into(),
                account: ACCOUNT.into(),
                scopes: vec!["provider.draft".into()],
                fingerprint: crate::secret::fingerprint(token),
                custody: custody.into(),
                exchange: "token".into(),
                enrolled_at: 1.0,
                by: "operator".into(),
            },
            false,
            None,
        )
        .unwrap();
    host_proof.config["account"] = json!(ACCOUNT);
    host_proof.config["connection_kind"] = json!("enrolled");
    host_proof.config["connection_id"] = json!("canonical868");
    host_proof.config["connection_revision"] = json!(1);
    assert!(
        shared.app_capability_quote(&host_proof).is_err(),
        "canonical bearer account cannot use hosted transport"
    );
    Arc::get_mut(&mut shared).unwrap().platforms.insert(
        "agenticos_external".into(),
        Arc::new(
            agenticos_external::AgenticosExternalAdapter::with_deployment_pin(
                "https://external.example.test",
                Some(agenticos_external::MANIFEST_PIN),
            )
            .unwrap(),
        ),
    );
    let config = shared
        .app_binding_config("install868", None, "image", "canonical868", &bundle, &files)
        .unwrap();
    assert_eq!(config["account"], ACCOUNT);
    assert_eq!(config["connection_kind"], "enrolled");
    assert_eq!(shared.app_capability_credential(&config).unwrap(), token);
    assert_eq!(
        crate::platform::load_credential(
            &shared.store,
            &shared.platform_custody,
            "agenticos_external",
            ACCOUNT
        )
        .unwrap(),
        token
    );
    // Missing custody cannot fall back to lease/empty bytes in external mode.
    shared.platform_custody.remove(custody, &key).unwrap();
    assert!(shared.app_capability_credential(&config).is_err());
    let missing = BindingProof {
        id: "canonical-proof".into(),
        revision: 1,
        digest: "synthetic".into(),
        config,
    };
    assert!(
        shared.app_capability_quote(&missing).is_err(),
        "missing external custody must refuse before price traffic"
    );
}

/// CAD-1096 redaction: only `[A-Za-z0-9_]{1,64}` after "refused: " reaches
/// the operator; the board re-checks the exact shape before relaying it.
#[test]
fn cad1096_quote_refusal_surfaces_only_a_bounded_code() {
    let bare = PRICE_REFUSED;
    let ok = "a".repeat(64);
    let shaped = format!("{bare}: {ok}");
    assert_eq!(price_refusal(&format!("door refused: {ok}")), shaped);
    assert_eq!(operator_price_refusal(&shaped), Some(shaped.as_str()));
    assert_eq!(operator_price_refusal(bare), Some(bare));
    let long = "a".repeat(65);
    for code in [
        long.as_str(),
        "n\u{f6}t_allowed",
        "a\nb",
        "a\n",
        r#"{"code":"x"}"#,
        "https://evil.test/x",
        "a b",
        "x refused: a b",
        "a: b",
        ":a",
        "",
        "\0",
    ] {
        assert_eq!(
            price_refusal(&format!("door refused: {code}")),
            bare,
            "{code:?}"
        );
        let relayed = format!("{bare}: {code}");
        assert_eq!(operator_price_refusal(&relayed), None, "{code:?}");
    }
    for message in [
        format!("{bare}x"),
        format!("{bare}:a"),
        format!("{bare}: a: b"),
        format!("x {bare}"),
    ] {
        assert_eq!(operator_price_refusal(&message), None, "{message:?}");
    }
    assert_eq!(price_refusal("provider quote is not JSON"), bare);
}

// CAD-1633 independent admission acceptance, written by a reviewer other than
// the implementer (AGENTS.md "Gates and security work"). It lives in this
// in-crate module, not `tests/`, because `pi_bash_tool_session` is `pub(super)`.
//
// Guard proven: a direct, live, session-leading bash/sh child of a managed
// pi-family endpoint is admitted, and family membership comes from the
// managed-family predicate, not the provider name. Refused: managed codex and
// pi/pty (outside the family), and a family row whose proof does not bind
// (tool pid is not the caller session, tool is not a session leader, or the
// tool executable is neither bash nor sh).

fn cad1633_shared() -> (tempfile::TempDir, Arc<Shared>) {
    let dir = tempfile::Builder::new()
        .prefix("cad1633")
        .tempdir()
        .unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    (dir, shared)
}

fn cad1633_register(shared: &Shared, cwd: &Path, alias: &str, provider: &str, kind: &str) {
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias,
            provider,
            endpoint_kind: kind,
            role: "worker",
            cwd: cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
}

fn cad1633_proof(alias: &str, root: u32, tool: u32) -> crate::slots::StrictCaller {
    crate::slots::StrictCaller {
        enrollment_id: "enr-cad1633".into(),
        lane: alias.into(),
        segment: vec![tool, root],
    }
}

/// A spawned process killed (with its group) on drop.
struct Tool(std::process::Child);

impl Drop for Tool {
    fn drop(&mut self) {
        let pid = self.0.id() as i32;
        // A session leader is its own process group: reap its children too.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cad1633_spawn(program: &str, args: &[&str], session_leader: bool) -> Tool {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if session_leader {
        // Mirrors pi's built-in bash tool: the spawned shell leads a new session.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    Tool(cmd.spawn().unwrap())
}

#[test]
fn cad1633_managed_omp_bash_tool_session_is_admitted_like_managed_pi() {
    let (dir, shared) = cad1633_shared();
    cad1633_register(&shared, dir.path(), "omp-worker", "omp", "managed");
    cad1633_register(&shared, dir.path(), "pi-worker", "pi", "managed");
    let root = std::process::id();
    for program in ["/bin/sh", "/bin/bash"] {
        let tool = cad1633_spawn(program, &["-c", "sleep 30; true"], true);
        let pid = tool.0.id();
        for alias in ["omp-worker", "pi-worker"] {
            let proof = cad1633_proof(alias, root, pid);
            let admitted = shared.pi_bash_tool_session(alias, &proof, pid).unwrap();
            assert!(
                admitted,
                "{alias} bash tool child via {program} must be admitted"
            );
        }
    }
}

#[test]
fn cad1633_non_family_endpoints_stay_refused_even_with_a_bound_bash_proof() {
    let (dir, shared) = cad1633_shared();
    cad1633_register(&shared, dir.path(), "codex-worker", "codex", "managed");
    cad1633_register(&shared, dir.path(), "pi-pty", "pi", "pty");
    let root = std::process::id();
    let tool = cad1633_spawn("/bin/sh", &["-c", "sleep 30; true"], true);
    let pid = tool.0.id();
    for alias in ["codex-worker", "pi-pty"] {
        let proof = cad1633_proof(alias, root, pid);
        let admitted = shared.pi_bash_tool_session(alias, &proof, pid).unwrap();
        assert!(
            !admitted,
            "{alias} is outside the pi family and must stay refused"
        );
    }
}

#[test]
fn cad1633_family_row_with_an_unbound_proof_stays_refused() {
    let (dir, shared) = cad1633_shared();
    cad1633_register(&shared, dir.path(), "omp-worker", "omp", "managed");
    let root = std::process::id();

    // Tool pid does not own the caller session: the caller is this test process.
    let tool = cad1633_spawn("/bin/sh", &["-c", "sleep 30; true"], true);
    let pid = tool.0.id();
    let proof = cad1633_proof("omp-worker", root, pid);
    assert!(
        !shared
            .pi_bash_tool_session("omp-worker", &proof, root)
            .unwrap(),
        "a caller session that is not the tool pid must be refused"
    );

    // Tool is not a session leader: same shell without setsid, caller session = its pid.
    let plain = cad1633_spawn("/bin/sh", &["-c", "sleep 30; true"], false);
    let plain_pid = plain.0.id();
    let proof = cad1633_proof("omp-worker", root, plain_pid);
    assert!(
        !shared
            .pi_bash_tool_session("omp-worker", &proof, plain_pid)
            .unwrap(),
        "a tool that is not its own session leader must be refused"
    );

    // Tool executable is not bash/sh: a session leader running sleep.
    let other = cad1633_spawn("/bin/sleep", &["30"], true);
    let other_pid = other.0.id();
    let proof = cad1633_proof("omp-worker", root, other_pid);
    assert!(
        !shared
            .pi_bash_tool_session("omp-worker", &proof, other_pid)
            .unwrap(),
        "a session-leading tool that is not bash/sh must be refused"
    );
}
