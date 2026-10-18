// Included in the existing native-chain module to reuse its isolated daemon,
// verified-TLS fixture, private credential handling and owned-process cleanup.
// No production transport, authorization or provider is replaced by this rig.

struct Cad1568WorkingDirectory(PathBuf);

impl Drop for Cad1568WorkingDirectory {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.0).unwrap();
    }
}

fn cad1568_peer(root: &Path, port: u16, rig: &Rig, cert: &Path, key: &Path, nonce: &str) -> Driver {
    let api = PathBuf::from(
        std::env::var_os("CAD1568_AOS_API_DIR").expect("controlled run needs CAD1568_AOS_API_DIR"),
    );
    assert!(api.is_absolute() && api.join("vitest.config.ts").is_file());
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cad1568");
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).unwrap();
    let pair = Ed25519KeyPair::from_seed_unchecked(&seed).unwrap();
    let keys = json!([{"kty":"OKP", "crv":"Ed25519", "kid":KID,
        "x":URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()), "d":URL_SAFE_NO_PAD.encode(seed)}])
    .to_string();
    let log = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("driver.log"))
        .unwrap();
    Driver {
        child: Command::new("node")
            .current_dir(&api)
            .arg(api.join("node_modules/vitest/vitest.mjs"))
            .args(["run", "--config"])
            .arg(api.join("vitest.config.ts"))
            .arg("--root")
            .arg(fixture)
            .args([
                "--globals",
                "--pool=forks",
                "--maxWorkers=1",
                "--no-file-parallelism",
                "src/messages.test.ts",
            ])
            .env("CAD1568_NATIVE_CHAIN", "1")
            .env("CAD1568_AOS_API_DIR", &api)
            .env("CAD1434_BRIDGE_PORT", port.to_string())
            .env(
                "CAD1434_NATIVE_ORIGIN",
                format!("http://127.0.0.1:{}", rig.port),
            )
            .env("CAD1434_BOARD_KEYS", keys)
            .env("CAD1434_TLS_CERT", cert)
            .env("CAD1434_TLS_KEY", key)
            .env("CAD1434_TOKEN_OUT", root.join("tokens.json"))
            .env("CAD1434_CONTROL_NONCE", nonce)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("spawn real hosted messaging peer"),
    }
}

#[test]
#[ignore = "controlled real AOS/native runtime; supply CAD1568_AOS_API_DIR and run under admission"]
fn cad1568_real_hosted_message_roundtrip_and_refusals() {
    // Match the executable's normal main-thread stack, not libtest's smaller
    // worker stack, while keeping the disposable TLS pin on the CLI thread.
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(cad1568_hosted_cli_scenario)
        .unwrap()
        .join()
        .unwrap();
}

fn cad1568_hosted_cli_scenario() {
    assert!(
        std::env::var_os("CAD1568_AOS_API_DIR").is_some(),
        "explicit AOS API checkout required"
    );
    let root = tempfile::Builder::new()
        .prefix("c1568-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    for dir in [
        "home", "xdg", "tmp", "locks", "owner", "member", "s", "master",
    ] {
        fs::create_dir(root.path().join(dir)).unwrap();
        fs::set_permissions(root.path().join(dir), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _environment = Environment::isolate(root.path());
    // An external CLI must not discover the checkout's production tracker.
    // Change only this test's cwd; retain all managed-caller identity pins.
    let _working_directory = Cad1568WorkingDirectory(std::env::current_dir().unwrap());
    std::env::set_current_dir(root.path()).unwrap();
    // Trusted synthetic endpoint bootstrap before the daemon owns the DB.
    // This does not claim master installation/model-policy acceptance. The
    // existing fake adapter completes real queue turns without paid calls.
    let seed = crate::store::Store::open(&root.path().join("s/cadence.sqlite3")).unwrap();
    let cwd = root.path().join("master");
    seed.register_agent(&crate::store::NewAgent {
        alias: "master",
        provider: "fake",
        endpoint_kind: "fake",
        role: "worker",
        cwd: cwd.to_str().unwrap(),
        sandbox: "read-only",
        instructions: None,
        params: None,
        team_role: None,
        model_policy: None,
    })
    .unwrap();
    seed.set_enabled("master", false).unwrap();
    drop(seed);
    let port = free_port(&[]);
    let issuer = format!("https://issuer.board.localhost:{port}");
    let rig = Rig::start(root.path(), &issuer);
    private_write(&root.path().join("native-seam"), rig.seam.as_bytes());
    let (ca_der, cert, key) = pki(root.path());
    let _trust = test_fixture::install(Fixture {
        ca_der,
        loopback: format!("127.0.0.1:{port}").parse().unwrap(),
    });
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).unwrap();
    let nonce = URL_SAFE_NO_PAD.encode(random);
    let mut driver = cad1568_peer(root.path(), port, &rig, &cert, &key, &nonce);
    let ready = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_ready(&mut driver, &root.path().join("tokens.json"))
    })) {
        Ok(ready) => ready,
        Err(panic) => {
            let diagnostics = root.path().with_extension("driver.log");
            let mut log = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&diagnostics)
                .unwrap();
            use std::io::Write;
            log.write_all(&fs::read(root.path().join("driver.log")).unwrap())
                .unwrap();
            eprintln!(
                "private peer-startup diagnostics: {}",
                diagnostics.display()
            );
            std::panic::resume_unwind(panic);
        }
    };
    assert_eq!(ready.issuer, issuer);
    assert_eq!(ready.audience, BOARD);
    assert_eq!(ready.owner["principal"]["subject_id"], ready.owner_id);
    assert_eq!(ready.member["principal"]["subject_id"], ready.member_id);
    let owner_dir = root.path().join("owner");
    let member_dir = root.path().join("member");
    for (answer, dir) in [(&ready.owner, &owner_dir), (&ready.member, &member_dir)] {
        crate::remote_enrollment::login_grant_for_test(answer, &issuer)
            .unwrap()
            .save_credential(dir)
            .unwrap();
        private_write(&dir.join("trusted-issuer"), issuer.as_bytes());
    }
    let cli_auth = crate::remote_auth::auth_dir(None).unwrap();
    fs::create_dir_all(&cli_auth).unwrap();
    let config_dir = cli_auth.parent().unwrap();
    for dir in [&cli_auth, config_dir] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    crate::remote_enrollment::login_grant_for_test(&ready.owner, &issuer)
        .unwrap()
        .save_credential(&cli_auth)
        .unwrap();
    private_write(&cli_auth.join("trusted-issuer"), issuer.as_bytes());
    private_write(
        &config_dir.join("orgs.json"),
        json!({
            "selected":{"org":"other"},
            "connections":[
                {
                    "selection":{"org":SLUG},
                    "destination":{"mode":"remote","endpoint":BOARD,"org_id":WORKSPACE}
                },
                {
                    "selection":{"org":"other"},
                    "destination":{
                        "mode":"remote",
                        "endpoint":"https://other.cadencecloud.app",
                        "org_id":"other_workspace"
                    }
                }
            ]
        })
        .to_string()
        .as_bytes(),
    );
    let target = RemoteTarget {
        identity: None,
        org: SLUG.into(),
        org_id: WORKSPACE.into(),
        endpoint: BOARD.into(),
    };
    // Prime only by fetching the REAL fixture issuer JWKS over verified TLS.
    // Handler signature/claim checks still run; fresh handler-thread TLS JWKS
    // fetching itself is not an acceptance claim of this existing harness.
    ui::prime_cli_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    daemon::prime_cli_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    let cli = |dir: &Path, verb: &str, args: Value| -> crate::Result<Value> {
        match call_verb_in(
            &target,
            verb,
            args.as_object().unwrap().clone(),
            60,
            dir,
            |_| {},
            std::thread::sleep,
        )? {
            RemoteAnswer::Ok(value) => Ok(value),
            RemoteAnswer::Conflict(value) => panic!("unexpected message conflict: {value}"),
        }
    };
    let send = |id: &str| json!({"alias":"master","text":"COMPACT","message":id});
    let receipt = cli(&owner_dir, "message_send", send("cad1568-roundtrip")).unwrap();
    assert_eq!(
        receipt["state"], "queued",
        "offline recipient must not look completed"
    );
    assert_eq!(receipt["duplicate"], false);
    assert_eq!(
        cli(&owner_dir, "message_send", send("cad1568-roundtrip")).unwrap()["duplicate"],
        true
    );
    let error = cli(&owner_dir, "message_send", send("cad1568-lost-receipt")).unwrap_err();
    assert_eq!(
        error.kind(),
        "unknown",
        "post-commit response loss is not a rejection"
    );
    assert_eq!(
        cli(
            &owner_dir,
            "message_read",
            json!({"message":"cad1568-lost-receipt"})
        )
        .unwrap()["state"],
        "queued"
    );
    assert_eq!(
        cli(&owner_dir, "message_send", send("cad1568-lost-receipt")).unwrap()["duplicate"],
        true
    );
    let member_denied =
        cli(&member_dir, "message_send", send("cad1568-member-refused")).unwrap_err();
    assert_eq!(member_denied.code(), Some("not_permitted"));
    let private_read = cli(
        &member_dir,
        "message_read",
        json!({"message":"cad1568-roundtrip"}),
    )
    .unwrap_err();
    assert_eq!(private_read.code(), Some("not_found"));
    let mut worker = send("cad1568-local-refused");
    worker["alias"] = json!("local-worker");
    assert_eq!(
        cli(&owner_dir, "message_send", worker).unwrap_err().code(),
        Some("recipient_class_unsupported")
    );
    // The same production clap parser and routed dispatch as the packaged
    // CLI, with the disposable CA pin confined to this library-test thread.
    assert_eq!(
        crate::cli::run_acceptance_argv(&[
            "cadence",
            "--org",
            SLUG,
            "send",
            "master",
            "--message",
            "cad1568-cli-roundtrip",
            "--text",
            "COMPACT",
        ])
        .unwrap(),
        0
    );
    rig.rpc("agent_resume", json!({"alias":"master"}));
    for id in [
        "cad1568-roundtrip",
        "cad1568-lost-receipt",
        "cad1568-cli-roundtrip",
    ] {
        let deadline = Instant::now() + Duration::from_secs(60);
        let completed = loop {
            let read = cli(&owner_dir, "message_read", json!({"message":id})).unwrap();
            if read["state"] == "completed" {
                break read;
            }
            assert!(
                !["failed", "unknown", "cancelled"]
                    .iter()
                    .any(|s| read["state"] == *s),
                "delivery failed: {read}"
            );
            assert!(Instant::now() < deadline, "delivery timed out: {read}");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(completed["result"]["text"], "FAKE_COMPACTED");
        assert!(!completed["awaiting_report"].as_bool().unwrap());
        assert!(completed.get("turn_id").is_none());
        assert!(completed["result"].get("turn_id").is_none());
    }
    assert_eq!(
        crate::cli::run_acceptance_argv(&[
            "cadence",
            "--org",
            SLUG,
            "message",
            "read",
            "cad1568-cli-roundtrip",
        ])
        .unwrap(),
        0
    );
    let owner = load_credential(&owner_dir, &target).unwrap();
    let envelope = authorize(&target, &owner, Surface::Cli, &["cli.read"], AUTH_TIMEOUT).unwrap();
    let read_request = json!({"version":CLI_VERSION,"envelope":envelope,"verb":"message_read", "arguments":{"message":"cad1568-roundtrip"}});
    let (status, _, _) = post(
        BOARD,
        "/__platform/cli/call",
        &owner.access_token,
        &read_request,
        CALL_TIMEOUT,
    )
    .unwrap();
    assert_eq!(status, 200);
    let (replay, _, _) = post(
        BOARD,
        "/__platform/cli/call",
        &owner.access_token,
        &read_request,
        CALL_TIMEOUT,
    )
    .unwrap();
    assert_eq!(replay, 401);
    let read_only = authorize(&target, &owner, Surface::Cli, &["cli.read"], AUTH_TIMEOUT).unwrap();
    let (scope_denied, _, scope_body) = post(BOARD, "/__platform/cli/call", &owner.access_token,
        &json!({"version":CLI_VERSION,"envelope":read_only,"verb":"message_send", "arguments":send("cad1568-no-write-grant")}), CALL_TIMEOUT).unwrap();
    // The platform distinguishes this permission failure by reason while
    // retaining its established 401 envelope-authority response status.
    assert_eq!(scope_denied, 401);
    assert_eq!(scope_body.unwrap()["error"]["reason"], "scope_not_granted");
    let (wrong_workspace, _, _) = post(BOARD, "/__platform/cli/authorize", &owner.access_token,
        &json!({"version":CLI_VERSION,"organization_id":"other-workspace","audience":BOARD, "requested_scopes":["cli.write"],"ttl_seconds":MAX_TTL}), AUTH_TIMEOUT).unwrap();
    assert!((400..500).contains(&wrong_workspace));
    let before_removal =
        authorize(&target, &owner, Surface::Cli, &["cli.write"], AUTH_TIMEOUT).unwrap();
    assert_eq!(control(port, &nonce, "revoke_owner_session"), 200);
    let (removed, _, _) = post(BOARD, "/__platform/cli/call", &owner.access_token,
        &json!({"version":CLI_VERSION,"envelope":before_removal,"verb":"message_send", "arguments":send("cad1568-removed-owner")}), CALL_TIMEOUT).unwrap();
    assert_eq!(removed, 401);
    for id in [
        "cad1568-member-refused",
        "cad1568-local-refused",
        "cad1568-no-write-grant",
        "cad1568-removed-owner",
    ] {
        assert!(
            scoped(Asserted::Operator, || client::rpc(
                &rig.state,
                "message_read",
                json!({"message":id})
            ))
            .is_err(),
            "refused send persisted {id}"
        );
    }
    assert_eq!(control(port, &nonce, "finish"), 200);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(success) = driver.status() {
            assert!(success, "real hosted peer failed");
            break;
        }
        assert!(Instant::now() < deadline, "real hosted peer did not settle");
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("CAD-1568 controlled hosted proof: queued -> synthetic completion -> owned readback; post-commit receipt loss reconciled/deduped; member/private/local/scope/workspace/replay/removed-member refusals held");
}
