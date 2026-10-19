// Included in the existing native-chain module to reuse its isolated daemon,
// verified-TLS fixture, private credential handling and owned-process cleanup.
// No production transport, authorization or provider is replaced by this rig.
//
// The user-facing journey runs in the actual `cadence` executable: the
// debug binary named by CAD1568_CLI, built `cargo build --features test-seam
// --bin cadence` (same main/parser/dispatch; the seam carries the env-armed
// fixture transport CADENCE_TEST_TLS_CA/CADENCE_TEST_TLS_CONNECT and is
// compile_error-excluded from release). TLS chain and hostname verification,
// the signed-envelope chain and every authority check remain real; only
// public DNS/CA selection is substituted, for the fixture's own two hosts.

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
            .env("CAD1568_REQUEST_LEDGER", root.join("requests.jsonl"))
            .env("CAD1434_CONTROL_NONCE", nonce)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("spawn real hosted messaging peer"),
    }
}

/// The packaged executable under proof plus the armed fixture transport it
/// needs. `command` builds each child as an external human CLI process: it
/// sees only the persona's own HOME/config and the fixture pair. env_clear
/// is the containment — and it is lawful only because this acceptance is an
/// external coordinator scenario that refuses to run under a managed caller
/// identity at all: a real CADENCE_ALIAS in the parent is a precondition
/// failure, never something the child env may quietly erase.
struct PackagedCli {
    bin: PathBuf,
    ca: PathBuf,
    connect: String,
}

impl PackagedCli {
    fn command(&self, home: &Path, config: &Path) -> Command {
        // Containment, not erasure: env_clear strips the parent's ordinary
        // environment, so a managed caller identity must be absent before
        // this runs — clearing one would be identity bypass, not isolation.
        assert!(
            std::env::var_os("CADENCE_ALIAS").is_none(),
            "packaged acceptance must not run under a managed caller identity"
        );
        let mut command = Command::new(&self.bin);
        command
            .env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", config)
            .env("TMPDIR", "/tmp")
            .env("PATH", "/usr/bin:/bin")
            .env("CADENCE_TEST_TLS_CA", &self.ca)
            .env("CADENCE_TEST_TLS_CONNECT", &self.connect);
        command
    }
    fn run(&self, home: &Path, config: &Path, argv: &[&str]) -> std::process::Output {
        crate::proc::run_bounded(
            self.command(home, config).args(argv),
            Duration::from_secs(120),
        )
        .unwrap_or_else(|_| panic!("packaged CLI timed out: cadence {}", argv.join(" ")))
    }
    fn ok(&self, home: &Path, config: &Path, argv: &[&str]) -> std::process::Output {
        let out = self.run(home, config, argv);
        assert!(
            out.status.success(),
            "packaged `cadence {}` failed: {}",
            argv.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    /// Human routing/reconciliation advice may precede the final JSON error,
    /// which carries the actual kind and stable wire code.
    fn error(&self, out: &std::process::Output) -> Value {
        assert!(
            !out.status.success(),
            "packaged CLI unexpectedly succeeded: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = out.stderr.as_slice();
        let start = if stderr.starts_with(b"{") {
            0
        } else {
            stderr
                .windows(2)
                .position(|pair| pair == b"\n{")
                .map(|offset| offset + 1)
                .expect("packaged CLI must emit a final JSON error object")
        };
        serde_json::from_slice(&stderr[start..]).unwrap_or_else(|_| {
            panic!(
                "packaged CLI error was not the contract JSON: {}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
    fn refusal(&self, out: &std::process::Output) -> String {
        self.error(out)["code"]
            .as_str()
            .expect("packaged CLI error carried no code")
            .to_owned()
    }
}

#[test]
#[ignore = "packaged CLI + controlled real AOS/native runtime; needs CAD1568_AOS_API_DIR and CAD1568_CLI (a `cargo build --features test-seam --bin cadence` debug binary) under admission"]
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

/// Every send receipt and readback must carry the verified nonsecret
/// binding: durable id, exact resolved recipient, immutable workspace,
/// canonical human subject, the retained display sender and the
/// conversation the enqueue fixed. A display handle is not proof of the
/// principal. Returns the message's fixed conversation so a duplicate or
/// readback can be matched to the same id's binding.
fn cad1568_assert_receipt(receipt: &Value, id: &str, subject: &str) -> String {
    assert_eq!(receipt["message"], id);
    assert_eq!(receipt["alias"], "master");
    assert_eq!(receipt["workspace_id"], WORKSPACE);
    assert_eq!(receipt["sender_subject"], subject);
    assert!(
        receipt["sender"].as_str().is_some_and(|s| !s.is_empty()),
        "receipt lost the display sender: {receipt}"
    );
    receipt["conversation"]
        .as_str()
        .unwrap_or_else(|| panic!("receipt lost the fixed Home conversation: {receipt}"))
        .to_owned()
}

/// The body each durable identity sends. The journey message references the
/// wiki page staged just before it, and the concurrent pair carry distinct
/// bodies — their terminal results must differ, never pass by swap.
fn cad1568_body(id: &str) -> &'static str {
    match id {
        "cad1568-packaged-roundtrip" => {
            "Review the staged tender kit at projects/lc/pages/cad1568-kit.md"
        }
        "cad1568-concurrent-a" => "First concurrent coordination note",
        "cad1568-concurrent-b" => "Second concurrent coordination note",
        _ => "COMPACT",
    }
}

/// The exact fake-provider terminal text the message's own body earns:
/// `COMPACT` completes `FAKE_COMPACTED`, anything else `FAKE_REPLY: <body>`.
/// Match on the echoed body, not a shared constant, so a swapped or
/// smuggled result cannot satisfy another id's readback.
fn cad1568_expected_result(id: &str) -> String {
    let body = cad1568_body(id);
    if body == "COMPACT" {
        "FAKE_COMPACTED".to_owned()
    } else {
        format!("FAKE_REPLY: {body}")
    }
}

/// The packaged send argv for one durable identity: explicit `--org lc`
/// workspace pin, exact canonical recipient, caller-chosen stable message
/// id and the body `cad1568_body` fixes for that id.
fn cad1568_send_argv(id: &str) -> Vec<&str> {
    vec![
        "--org",
        SLUG,
        "send",
        "master",
        "--message",
        id,
        "--text",
        cad1568_body(id),
    ]
}

/// The hosted boundary's own record of the requests it was asked to serve:
/// method, path and status only. The peer appends one JSON line per
/// proxied request, so the controller can name the hop that actually
/// handled a request instead of inferring it from a refusal.
fn hosted_requests(root: &Path) -> Vec<Value> {
    let Ok(text) = fs::read_to_string(root.join("requests.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

/// The CLI-surface requests that reached the hosted boundary. A request
/// that is absent from this record never left the client.
fn hosted_cli_requests(root: &Path) -> Vec<Value> {
    hosted_requests(root)
        .into_iter()
        .filter(|entry| {
            entry
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| path.starts_with("/__platform/cli/"))
        })
        .collect()
}

/// The durable rows this runtime actually holds for `master`, read with
/// the runtime's own operator-scoped listing RPC. Membership in this set
/// is the observation: a refused id is simply not among the rows, which
/// no read error can stand in for.
fn durable_master_ids(rig: &Rig) -> std::collections::BTreeSet<String> {
    let listed = scoped(Asserted::Operator, || {
        client::rpc(
            &rig.state,
            "message_list",
            json!({"to": "master", "limit": 50}),
        )
    })
    .unwrap_or_else(|error| panic!("durable row listing failed: {error}"));
    let rows = listed
        .get("messages")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("the durable listing returned no rows: {listed}"));
    rows.iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// What the runtime's own store holds for one durable id.
enum DurableRow {
    Present,
    Absent,
}

/// The runtime's own store, read with operator scope. The store's own
/// absent-row answer is the only answer this probe may read as absence:
/// a transport, deserialization, internal or store failure is a failed
/// probe and stops the check — it is never evidence that a row does not
/// exist, and an authorization answer is never absence either.
fn durable_row(rig: &Rig, id: &str) -> DurableRow {
    match scoped(Asserted::Operator, || {
        client::rpc(&rig.state, "message_read", json!({"message": id}))
    }) {
        Ok(_) => DurableRow::Present,
        Err(error) => {
            let text = error.to_string();
            if error.kind() == "rejected"
                && text.contains("No such message")
                && text.contains(id)
            {
                DurableRow::Absent
            } else {
                panic!("the store probe for {id} did not answer: {error}");
            }
        }
    }
}

fn cad1568_hosted_cli_scenario() {
    let bin = PathBuf::from(std::env::var_os("CAD1568_CLI").expect(
        "packaged acceptance needs CAD1568_CLI=/absolute/path/to/target/debug/cadence \
             built with `--features test-seam`",
    ));
    assert!(
        bin.is_absolute() && bin.is_file(),
        "CAD1568_CLI must name the built cadence executable"
    );
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
        "home",
        "xdg",
        "tmp",
        "locks",
        "s",
        "master",
        "owner-home",
        "owner-cfg",
        "member-home",
        "member-cfg",
        "neg-cfg",
        "wrong",
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
    let port = (3110..=3199)
        .rev()
        .find(|port| std::net::TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("no free controlled TLS fixture port");
    let issuer = format!("https://issuer.board.localhost:{port}");
    let rig = Rig::start(root.path(), &issuer);
    private_write(&root.path().join("native-seam"), rig.seam.as_bytes());
    let (ca_der, cert, key) = pki(root.path());
    // The CLI reads the CA file it is pointed at — private, like the rest of
    // the fixture's key material.
    fs::set_permissions(
        root.path().join("ca.pem"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
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
    let owner_home = root.path().join("owner-home");
    let owner_cfg = root.path().join("owner-cfg");
    let member_home = root.path().join("member-home");
    let member_cfg = root.path().join("member-cfg");
    let owner_auth = owner_cfg.join("cadence/remote-auth");
    let member_auth = member_cfg.join("cadence/remote-auth");
    // Private config/credential directories as a real first login leaves
    // them: each persona's own XDG config, `cadence/` and `remote-auth/`.
    for dir in [
        &owner_cfg.join("cadence"),
        &member_cfg.join("cadence"),
        &owner_auth,
        &member_auth,
    ] {
        fs::create_dir_all(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    // The member home carries its real seeded grant (fixture-synthetic
    // consent, as before). The owner's packaged `cadence login` below is the
    // ticketed journey and earns its own credential — nothing owner-side is
    // pre-seeded.
    crate::remote_enrollment::login_grant_for_test(&ready.member, &issuer)
        .expect("member workspace role is accepted")
        .save_credential(&member_auth)
        .unwrap();
    private_write(&member_auth.join("trusted-issuer"), issuer.as_bytes());
    private_write(
        &member_cfg.join("cadence/orgs.json"),
        json!({
            "selected":{"org":SLUG},
            "connections":[
                {
                    "selection":{"org":SLUG},
                    "destination":{"mode":"remote","endpoint":BOARD,"org_id":WORKSPACE}
                }
            ]
        })
        .to_string()
        .as_bytes(),
    );
    // Prime only by fetching the REAL fixture issuer JWKS over verified TLS.
    // Handler signature/claim checks still run; fresh handler-thread TLS JWKS
    // fetching itself is not an acceptance claim of this existing harness.
    ui::prime_cli_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    daemon::prime_cli_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    ui::prime_jwks_for_test(&issuer, KID, crate::issue::time::now_epoch()).unwrap();
    let ca_pem = root.path().join("ca.pem");
    let connect = format!("127.0.0.1:{port}");
    let cli_exe = PackagedCli {
        bin,
        ca: ca_pem.clone(),
        connect,
    };
    let target = RemoteTarget {
        identity: None,
        org: SLUG.into(),
        org_id: WORKSPACE.into(),
        endpoint: BOARD.into(),
    };

    // The pinned issuer is an operator precondition the packaged login
    // itself enforces; it is written before the child runs, never by it.
    private_write(&owner_auth.join("trusted-issuer"), issuer.as_bytes());
    // Packaged login: real `cadence login` against the real device-grant
    // handlers. The fixture auto-approves the issued user_code through the
    // production /v2 device/approve route under the owner's browser session —
    // consent is the real handler; only the human's click is synthetic.
    let login = cli_exe.ok(
        &owner_home,
        &owner_cfg,
        &[
            "login",
            "lc",
            "--issuer",
            issuer.as_str(),
            "--no-open",
            "--use",
        ],
    );
    assert!(
        String::from_utf8_lossy(&login.stdout).contains("Signed in to lc"),
        "packaged login did not report the verified workspace"
    );
    // The CLI's own registry writes land: the credential file and the remote
    // connection both come from the issuer-verified grant.
    let owner_orgs_path = owner_cfg.join("cadence/orgs.json");
    let registry: Value = serde_json::from_slice(&fs::read(&owner_orgs_path).unwrap()).unwrap();
    assert_eq!(registry["selected"]["org"], SLUG);
    assert_eq!(
        registry["connections"][0]["destination"],
        json!({"mode":"remote","endpoint":BOARD,"org_id":WORKSPACE})
    );
    let owner = load_credential(&owner_auth, &target).unwrap();
    assert_eq!(owner.subject_id.as_deref(), Some(ready.owner_id.as_str()));
    assert!(owner.cli_session.is_some());

    // A different saved remote default must not win over the explicit
    // `--org lc` (or a stray CADENCE_ORG) on any later command. `other`
    // points at the same real board handlers with a workspace this principal
    // is not a member of, so its refusals are also real. Overwrite in place:
    // a private owner-mode file like the CLI's own write leaves it.
    fs::write(
        &owner_orgs_path,
        json!({
            "selected":{"org":"other"},
            "connections":[
                {
                    "selection":{"org":SLUG},
                    "destination":{"mode":"remote","endpoint":BOARD,"org_id":WORKSPACE}
                },
                {
                    "selection":{"org":"other"},
                    "destination":{"mode":"remote","endpoint":BOARD,"org_id":"other_workspace"}
                }
            ]
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    fs::set_permissions(&owner_orgs_path, fs::Permissions::from_mode(0o600)).unwrap();

    // Harness bookkeeping path into the same real board: durable-message and
    // receipt assertions between packaged invocations use the library client
    // the CLI itself is built on, never a second transport.
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
    let send = |id: &str| json!({"alias":"master","text":cad1568_body(id),"message":id});
    // Record each message's receipt binding; the contract fixes the
    // conversation per message (stable across duplicates and readback), it
    // does not promise different sends share one thread.
    let mut conversations = std::collections::BTreeMap::new();

    // Stage the reference kit the main coordination send then names: real
    // `cadence wiki put`/`cat` through the hosted handlers.
    let kit_path = "projects/lc/pages/cad1568-kit.md";
    cli_exe.ok(
        &owner_home,
        &owner_cfg,
        &[
            "--org",
            SLUG,
            "wiki",
            "put",
            kit_path,
            "-m",
            "CAD-1568 synthetic tender kit",
        ],
    );
    let kit = cli_exe.ok(
        &owner_home,
        &owner_cfg,
        &["--org", SLUG, "wiki", "cat", kit_path],
    );
    assert_eq!(
        String::from_utf8(kit.stdout).unwrap(),
        "CAD-1568 synthetic tender kit"
    );

    // Offline recipient: the packaged send — whose body references the kit
    // staged just above — returns the durable queued receipt, never a
    // fabricated running/completed state.
    let first: Value = serde_json::from_slice(
        &cli_exe
            .ok(
                &owner_home,
                &owner_cfg,
                &cad1568_send_argv("cad1568-packaged-roundtrip"),
            )
            .stdout,
    )
    .unwrap();
    assert_eq!(
        first["state"], "queued",
        "offline recipient must not look completed"
    );
    assert_eq!(first["duplicate"], false);
    conversations.insert(
        "cad1568-packaged-roundtrip".to_owned(),
        cad1568_assert_receipt(&first, "cad1568-packaged-roundtrip", &ready.owner_id),
    );
    // Identical retry dedupes instead of enqueuing twice — here with a
    // stray CADENCE_ORG=other in the child env, which `--org lc` must beat.
    let mut command = cli_exe.command(&owner_home, &owner_cfg);
    command.env("CADENCE_ORG", "other");
    let again: Value = serde_json::from_slice(
        &crate::proc::run_bounded(
            command.args(cad1568_send_argv("cad1568-packaged-roundtrip")),
            Duration::from_secs(120),
        )
        .unwrap()
        .stdout,
    )
    .unwrap();
    assert_eq!(again["duplicate"], true);
    assert_eq!(
        again["conversation"],
        conversations["cad1568-packaged-roundtrip"]
    );

    // Two concurrent durable identities with distinct bodies, enqueued
    // before the recipient is ever resumed; each later completes with its
    // own correlated readback — a swapped result cannot satisfy either.
    for id in ["cad1568-concurrent-a", "cad1568-concurrent-b"] {
        let receipt: Value = serde_json::from_slice(
            &cli_exe
                .ok(&owner_home, &owner_cfg, &cad1568_send_argv(id))
                .stdout,
        )
        .unwrap();
        assert_eq!(receipt["state"], "queued");
        assert_eq!(receipt["duplicate"], false);
        conversations.insert(
            id.to_owned(),
            cad1568_assert_receipt(&receipt, id, &ready.owner_id),
        );
    }

    // Post-commit response loss is outcome-unknown to the packaged CLI,
    // never a rejection. The same read command reconciles the durable row
    // and the exact retry does not enqueue a second message.
    let lost = cli_exe.run(
        &owner_home,
        &owner_cfg,
        &cad1568_send_argv("cad1568-lost-receipt"),
    );
    assert!(!lost.status.success());
    let lost_err = cli_exe.error(&lost);
    assert_eq!(
        lost_err["kind"], "unknown",
        "post-commit response loss is not a rejection"
    );
    let reconciled: Value = serde_json::from_slice(
        &cli_exe
            .ok(
                &owner_home,
                &owner_cfg,
                &["--org", SLUG, "message", "read", "cad1568-lost-receipt"],
            )
            .stdout,
    )
    .unwrap();
    assert_eq!(reconciled["state"], "queued");
    assert_eq!(reconciled["workspace_id"], WORKSPACE);
    conversations.insert(
        "cad1568-lost-receipt".to_owned(),
        cad1568_assert_receipt(&reconciled, "cad1568-lost-receipt", &ready.owner_id),
    );
    let resent: Value = serde_json::from_slice(
        &cli_exe
            .ok(
                &owner_home,
                &owner_cfg,
                &cad1568_send_argv("cad1568-lost-receipt"),
            )
            .stdout,
    )
    .unwrap();
    assert_eq!(resent["duplicate"], true);
    assert_eq!(
        resent["conversation"],
        conversations["cad1568-lost-receipt"]
    );

    // A conflicting id/content pair is refused and writes nothing.
    let mut conflict = send("cad1568-packaged-roundtrip");
    conflict["text"] = json!("DIFFERENT");
    let error = cli(&owner_auth, "message_send", conflict).unwrap_err();
    assert_eq!(error.code(), Some("message_id_conflict"));

    // Recipient-class boundary through the packaged CLI: the request is
    // forwarded, and the container's own recipient-class guard refuses it
    // with its typed code — that code is the native runtime's, not the
    // board's (a board refusal names `error.hop = "board"` and a reason
    // instead), and the refusal is never a silent reroute.
    let before_local = hosted_cli_requests(root.path());
    let local_refused = cli_exe.run(
        &owner_home,
        &owner_cfg,
        &[
            "--org",
            SLUG,
            "send",
            "local-worker",
            "--message",
            "cad1568-local-refused",
            "--text",
            "COMPACT",
        ],
    );
    assert_eq!(
        cli_exe.refusal(&local_refused),
        "recipient_class_unsupported"
    );
    assert!(
        hosted_cli_requests(root.path()).len() > before_local.len(),
        "the local-recipient send must be forwarded before the container refuses it"
    );

    // Member home, packaged CLI: the request is forwarded, and the
    // container's role guard refuses it — master coordination needs a
    // verified human workspace owner. Its code is the container's own
    // (`not_permitted` is minted only by the container's `cli_recipient`;
    // a board refusal names `error.hop = "board"` and a reason instead).
    // The member's own read of the owner's message is privacy evidence:
    // it is refused as not found and proves nothing about which rows
    // exist, so it is never used as an enqueue-absence oracle.
    let before_member = hosted_cli_requests(root.path());
    let member_denied = cli_exe.run(
        &member_home,
        &member_cfg,
        &cad1568_send_argv("cad1568-member-refused"),
    );
    assert_eq!(cli_exe.refusal(&member_denied), "not_permitted");
    assert!(
        hosted_cli_requests(root.path()).len() > before_member.len(),
        "the member's send must be forwarded before the container refuses it"
    );
    let private_read = cli_exe.run(
        &member_home,
        &member_cfg,
        &[
            "--org",
            SLUG,
            "message",
            "read",
            "cad1568-packaged-roundtrip",
        ],
    );
    assert_eq!(cli_exe.refusal(&private_read), "not_found");

    // Wrong-workspace selection, case 1: the LOCAL immutable-workspace
    // credential pin. The `other` connection names a workspace this
    // principal holds no credential for, and the saved credential for
    // that org name is planted first: it names the lc workspace while
    // the destination claims another one. That is the state the pin
    // exists for — a destination changed under a credential that belongs
    // somewhere else — and the pin must refuse it on the client, before
    // anything is forwarded. The pin reads the workspace binding before
    // the token, so the planted bearer is never examined or sent.
    private_write(
        &owner_auth.join("cli-other.json"),
        json!({
            "version": "hosted-cadence-auth.v1",
            "organization_id": WORKSPACE,
            "organization_slug": SLUG,
            "audience": BOARD,
            "issuer": issuer,
            "subject_id": ready.owner_id,
            "capabilities": ["cli.read", "cli.write"],
            "access_token": format!("hct_{}", "P".repeat(43)),
            "expires_at": crate::issue::time::now_epoch() + 3600,
        })
        .to_string()
        .as_bytes(),
    );
    let before_pin = hosted_cli_requests(root.path());
    let wrong_workspace = cli_exe.run(
        &owner_home,
        &owner_cfg,
        &[
            "--org",
            "other",
            "send",
            "master",
            "--message",
            "cad1568-other-workspace",
            "--text",
            "COMPACT",
        ],
    );
    assert!(!wrong_workspace.status.success());
    // The hop that refused, named by what happened rather than by prose:
    // a local rejection carries no wire code, and no CLI request reached
    // the hosted boundary. The server was never asked, so this is not a
    // server-side wrong-workspace refusal.
    let pin_error = cli_exe.error(&wrong_workspace);
    assert_eq!(pin_error["kind"], "rejected");
    assert!(
        pin_error.get("code").is_none(),
        "the wrong-workspace refusal must be the local pin, not a hosted wire refusal: \
         {pin_error}"
    );
    let after_pin = hosted_cli_requests(root.path());
    assert_eq!(
        after_pin.len(),
        before_pin.len(),
        "the local credential pin must refuse before any HTTP forwarding: {after_pin:?}"
    );

    // The armed transport must fail closed, not silently degrade to
    // production DNS/trust: a valid private CA that is not the fixture's
    // refuses the chain, an unrelated fixture-family host is never
    // resolved, and a half-armed pair is a loud failure.
    pki(&root.path().join("wrong"));
    fs::set_permissions(
        root.path().join("wrong/ca.pem"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut wrong_ca = cli_exe.command(&owner_home, &owner_cfg);
    wrong_ca.env("CADENCE_TEST_TLS_CA", root.path().join("wrong/ca.pem"));
    let wrong_ca_out = crate::proc::run_bounded(
        wrong_ca.args([
            "--org",
            SLUG,
            "message",
            "read",
            "cad1568-packaged-roundtrip",
        ]),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        !wrong_ca_out.status.success(),
        "a valid-but-foreign CA must not reach the fixture"
    );
    let neg_auth = root.path().join("neg-cfg/cadence/remote-auth");
    fs::create_dir_all(&neg_auth).unwrap();
    fs::set_permissions(&neg_auth, fs::Permissions::from_mode(0o700)).unwrap();
    private_write(
        &neg_auth.join("trusted-issuer"),
        format!("https://unrelated.board.localhost:{port}").as_bytes(),
    );
    let foreign = cli_exe.run(
        &owner_home,
        &root.path().join("neg-cfg"),
        &[
            "login",
            "--issuer",
            &format!("https://unrelated.board.localhost:{port}"),
            "--no-open",
        ],
    );
    assert!(
        !foreign.status.success(),
        "an unrelated fixture-family host must never resolve"
    );
    let mut half_armed = cli_exe.command(&owner_home, &owner_cfg);
    half_armed.env_remove("CADENCE_TEST_TLS_CONNECT");
    let half = crate::proc::run_bounded(
        half_armed.args(["--org", SLUG, "message", "read", "x"]),
        Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        !half.status.success(),
        "a partial fixture arm must fail, never fall back to production DNS"
    );

    // Resume the recipient: every queued identity runs its own turn to the
    // result its own body earns — a swapped outcome fails the per-id check.
    rig.rpc("agent_resume", json!({"alias":"master"}));
    for id in [
        "cad1568-packaged-roundtrip",
        "cad1568-concurrent-a",
        "cad1568-concurrent-b",
        "cad1568-lost-receipt",
    ] {
        let deadline = Instant::now() + Duration::from_secs(60);
        let completed = loop {
            let read = cli(&owner_auth, "message_read", json!({"message":id})).unwrap();
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
        let result_text = completed["result"]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{id} completed with no result text: {completed}"));
        assert!(
            result_text.contains(&cad1568_expected_result(id)),
            "{id} terminal result is not its own body's reply: {result_text}"
        );
        assert_eq!(completed["workspace_id"], WORKSPACE);
        assert_eq!(completed["sender_subject"], ready.owner_id.as_str());
        assert_eq!(completed["conversation"], conversations[id]);
        assert!(!completed["awaiting_report"].as_bool().unwrap());
        assert!(completed.get("turn_id").is_none());
        assert!(completed["result"].get("turn_id").is_none());
    }
    // The exact terminal-result read the ticket documents, packaged.
    let terminal: Value = serde_json::from_slice(
        &cli_exe
            .ok(
                &owner_home,
                &owner_cfg,
                &[
                    "--org",
                    SLUG,
                    "message",
                    "read",
                    "cad1568-packaged-roundtrip",
                ],
            )
            .stdout,
    )
    .unwrap();
    assert_eq!(terminal["state"], "completed");
    assert!(
        terminal["result"]["text"].as_str().is_some_and(
            |text| text.contains(&cad1568_expected_result("cad1568-packaged-roundtrip"))
        ),
        "terminal readback does not carry the wiki-referencing send's own reply: {terminal}"
    );
    assert_eq!(
        terminal["conversation"],
        conversations["cad1568-packaged-roundtrip"]
    );

    // Signed-envelope authority checks stay on the direct wire, and each
    // one names the hop that refused it: the container's single-use
    // envelope check refuses the replayed answer, the board's scope check
    // refuses the under-scoped envelope, and the board's wrong-workspace
    // guard refuses an authorize that names another workspace. Each
    // refusal says which hop produced it — the board's own refusals
    // carry `error.hop = "board"` and a reason, the container's carry
    // their own typed code — so a forwarded status is never counted as
    // another hop's evidence.
    let envelope = authorize(&target, &owner, Surface::Cli, &["cli.read"], AUTH_TIMEOUT).unwrap();
    let read_request = json!({"version":CLI_VERSION,"envelope":envelope,"verb":"message_read", "arguments":{"message":"cad1568-packaged-roundtrip"}});
    let (status, _, _) = post(
        BOARD,
        "/__platform/cli/call",
        &owner.access_token,
        &read_request,
        CALL_TIMEOUT,
    )
    .unwrap();
    assert_eq!(status, 200);
    let (replay, _, replay_body) = post(
        BOARD,
        "/__platform/cli/call",
        &owner.access_token,
        &read_request,
        CALL_TIMEOUT,
    )
    .unwrap();
    assert_eq!(replay, 401);
    // The replay is refused by the CONTAINER's single-use envelope check
    // (ui/cli_route.rs::post consumes the jti and answers
    // `assertion_replayed`), after the board forwarded the still valid
    // envelope. The typed code in the relayed body is what binds this
    // refusal to that hop: a status alone cannot, and a board refusal
    // carries `error.hop = "board"` with a reason instead of a code.
    let replay_error = replay_body.unwrap()["error"].clone();
    assert_eq!(replay_error["code"], "assertion_replayed");
    assert!(
        replay_error.get("hop").is_none(),
        "the replay must be refused by the container, not by the board: {replay_error}"
    );
    let read_only = authorize(&target, &owner, Surface::Cli, &["cli.read"], AUTH_TIMEOUT).unwrap();
    let (scope_denied, _, scope_body) = post(BOARD, "/__platform/cli/call", &owner.access_token,
        &json!({"version":CLI_VERSION,"envelope":read_only,"verb":"message_send", "arguments":send("cad1568-no-write-grant")}), CALL_TIMEOUT).unwrap();
    // The platform distinguishes this permission failure by reason while
    // retaining its established 401 envelope-authority response status.
    // The refusal names the hop it came from, so it is the board's own
    // scope check and not a forwarded container refusal.
    assert_eq!(scope_denied, 401);
    let scope_error = scope_body.unwrap()["error"].clone();
    assert_eq!(scope_error["hop"], "board");
    assert_eq!(scope_error["reason"], "scope_not_granted");
    // The server-side wrong-workspace guard: a correctly authenticated
    // request that names another workspace reaches the hosted boundary
    // and is refused there by the board's own guard — its refusal names
    // the hop it came from. No envelope is minted, so no send can follow
    // it; the durable rows below show that nothing new exists.
    let before_wrong_org = hosted_cli_requests(root.path());
    let (wrong_org, _, wrong_org_body) = post(BOARD, "/__platform/cli/authorize", &owner.access_token,
        &json!({"version":CLI_VERSION,"organization_id":"other-workspace","audience":BOARD, "requested_scopes":["cli.write"],"ttl_seconds":MAX_TTL}), AUTH_TIMEOUT).unwrap();
    assert!((400..500).contains(&wrong_org));
    let wrong_org_error = wrong_org_body.unwrap()["error"].clone();
    assert_eq!(
        wrong_org_error["hop"], "board",
        "the wrong-workspace authorize must be refused by the board's own guard: \
         {wrong_org_error}"
    );
    let after_wrong_org = hosted_cli_requests(root.path());
    let wrong_org_requests = &after_wrong_org[before_wrong_org.len()..];
    assert!(
        wrong_org_requests.iter().any(|entry| {
            entry.get("path").and_then(Value::as_str) == Some("/__platform/cli/authorize")
                && entry
                    .get("status")
                    .and_then(Value::as_u64)
                    .is_some_and(|status| (400..500).contains(&status))
        }),
        "the wrong-workspace authorize must reach the hosted guard and be refused there: \
         {wrong_org_requests:?}"
    );
    let before_removal =
        authorize(&target, &owner, Surface::Cli, &["cli.write"], AUTH_TIMEOUT).unwrap();
    assert_eq!(control(port, &nonce, "revoke_owner_session"), 200);
    // Membership removed while the credential and the signed envelope are
    // both retained: the request reaches the hosted boundary and is
    // refused there for the now-dead membership, not on the client. The
    // refusal names the hop that produced it, the ledger shows it
    // reached the boundary, and the durable rows below show that this
    // attempted id was never stored.
    let before_removed = hosted_cli_requests(root.path());
    let (removed, _, removed_body) = post(BOARD, "/__platform/cli/call", &owner.access_token,
        &json!({"version":CLI_VERSION,"envelope":before_removal,"verb":"message_send", "arguments":send("cad1568-removed-owner")}), CALL_TIMEOUT).unwrap();
    assert_eq!(removed, 401);
    let removed_error = removed_body.unwrap()["error"].clone();
    assert_eq!(
        removed_error["hop"], "board",
        "the retained-envelope call must be refused by the board's live-membership check: \
         {removed_error}"
    );
    let after_removed = hosted_cli_requests(root.path());
    let removed_requests = &after_removed[before_removed.len()..];
    assert!(
        removed_requests.iter().any(|entry| {
            entry
                .get("status")
                .and_then(Value::as_u64)
                .is_some_and(|status| (400..500).contains(&status))
        }),
        "the retained-envelope call must reach the hosted authorization hop: \
         {removed_requests:?}"
    );
    // And the packaged CLI itself cannot use the revoked login. Its
    // refusal happens at the hosted authorization hop: the bearer no
    // longer resolves, so the request is refused there — the request
    // ledger shows it reached the boundary. The owner's own read cannot
    // stand in for absence here (that read is refused for authorization
    // now, which says nothing about rows), so the row is observed in the
    // durable store below.
    let before_revoked = hosted_cli_requests(root.path());
    let revoked = cli_exe.run(
        &owner_home,
        &owner_cfg,
        &cad1568_send_argv("cad1568-revoked-cli"),
    );
    assert!(!revoked.status.success());
    let after_revoked = hosted_cli_requests(root.path());
    let revoked_requests = &after_revoked[before_revoked.len()..];
    assert!(
        revoked_requests.iter().any(|entry| {
            entry
                .get("status")
                .and_then(Value::as_u64)
                .is_some_and(|status| (400..500).contains(&status))
        }),
        "the revoked send must reach the hosted authorization hop and be refused there: \
         {revoked_requests:?}"
    );
    // The durable rows the runtime actually holds, read with the
    // runtime's own operator-scoped listing: a refused send is absent
    // from this set, and a send that really happened is present in it.
    // Each id is listed with the hop that refused it, so no refusal is
    // counted as another hop's evidence.
    let durable = durable_master_ids(&rig);
    assert!(
        durable.contains("cad1568-packaged-roundtrip"),
        "the durable listing must see the message that really was enqueued: {durable:?}"
    );
    for (id, hop) in [
        ("cad1568-member-refused", "container role guard"),
        ("cad1568-no-write-grant", "board scope check"),
        ("cad1568-removed-owner", "board live-membership re-check"),
        ("cad1568-revoked-cli", "board authorization hop"),
        ("cad1568-other-workspace", "local credential pin"),
    ] {
        assert!(
            !durable.contains(id),
            "the {hop} refused {id} but a durable row exists"
        );
    }
    // The local recipient is not a listable agent, so its row is probed in
    // the store directly. The store's own absent-row answer is the only
    // absence this probe may read; a failed probe stops the check.
    assert!(
        matches!(
            durable_row(&rig, "cad1568-local-refused"),
            DurableRow::Absent
        ),
        "the container's recipient-class guard refused cad1568-local-refused but a durable row \
         exists"
    );
    assert!(
        matches!(
            durable_row(&rig, "cad1568-packaged-roundtrip"),
            DurableRow::Present
        ),
        "the store probe must see the message that really was enqueued"
    );
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
    eprintln!("CAD-1568 packaged-CLI proof: real login -> staged wiki -> --org send naming the kit with workspace/subject/conversation receipt -> own terminal result; distinct concurrent ids -> distinct results; offline queue; lost-receipt reconcile/dedupe; refusals held at the hop each one exercised (local credential pin; the board's scope, live-membership and authorization checks; the container's own recipient-class, role and single-use-envelope guards) with the durable rows observed absent in the runtime's own store");
}
