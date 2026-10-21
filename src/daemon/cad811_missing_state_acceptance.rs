//! Independent CAD-811 Spec F1 acceptance, authored without executing product.
//! CHILD of independent_public_owner_attribution; reuse its real Native fixture.
//! Exactly one bounded read-outcome/refusal case; no providers or sends.
use super::*;

const MISSING: &str = "proposal-only-missing";
const PRESENT: &str = "saved-control";
const CORRUPT: &str = "corrupt-control";
const FILTER: &str = "cad811_missing_freeze_content_are_absent_only_under_live_native_authority";

fn selector(operation: &str, id: &str) -> Value {
    match operation {
        "audience.show" => json!({"freeze_id": id}),
        "content.show" => json!({"campaign_id": id}),
        _ => panic!("unsupported independent selector"),
    }
}

fn method(operation: &str) -> &'static str {
    match operation {
        "audience.show" => "app_audience_show",
        "content.show" => "app_content_show",
        _ => panic!("unsupported independent method"),
    }
}

fn native_args(scope: &Scope, args: Value) -> Value {
    let mut args = args;
    args["install_id"] = json!(scope.install);
    args["context_id"] = json!(scope.context);
    args
}

fn read_rpc(native: &Native, session: &Session, body: Value) -> crate::Result<Value> {
    native.rpc(
        "app_screen_service_call",
        native.credentials(session, "public", body),
    )
}

fn error_only(result: crate::Result<Value>, label: &str) -> crate::Error {
    match result {
        Err(error) => error,
        Ok(_) => panic!("{label}: error became success"),
    }
}

fn no_resource(reply: &Value, label: &str) {
    assert!(
        reply.get("content").is_none() && reply.get("freeze").is_none(),
        "{label}: refusal exposed a successful resource payload"
    );
}

fn not_absence(error: &crate::Error, absence: Option<&str>, label: &str) {
    if let Some(absence) = absence {
        assert_ne!(
            error.code(),
            Some(absence),
            "{label}: refusal became absence"
        );
    }
}

fn denied(
    native: &Native,
    session: &Session,
    who: Asserted,
    body: Value,
    absence: Option<&str>,
    label: &str,
) {
    let error = error_only(
        native.fx.rpc(
            who.clone(),
            "app_screen_service_call",
            native.credentials(session, "public", body.clone()),
        ),
        label,
    );
    assert!(
        matches!(error.kind(), "rejected" | "gate" | "conflict"),
        "{label}: transport/internal failure is not guard evidence"
    );
    not_absence(&error, absence, label);
    let (status, reply) = native.fx.http(&who.as_str(), session, &body);
    assert!(
        matches!(status, 400 | 401 | 403 | 409),
        "{label}: must refuse, not success/absence/transport failure ({status})"
    );
    no_resource(&reply, label);
    if let Some(absence) = absence {
        assert_ne!(
            reply["code"].as_str(),
            Some(absence),
            "{label}: HTTP refusal became typed absence"
        );
    }
}

fn count(native: &Native, scope: &Scope, sql: &str, id: &str) -> i64 {
    let db = native.record_db(scope); // inherited read-only constructor
    let mut query = must(db.prepare(sql), "read-only absence observation");
    must(
        query.query_row(params![scope.context, id], |row| row.get(0)),
        "read-only absence count",
    )
}

fn second_owner(native: &Native) -> Session {
    let now = native.shared.operator_now();
    let opened = must(
        native.shared.operator_auth().open_public_bound(
            BoardUser {
                sub: SECOND_OWNER.into(),
                email: "c811-owner@example.test".into(),
                name: "Synthetic CRM owner".into(),
                role: "operator".into(),
                handle: OWNER.into(),
            },
            "c811-missing-other-jti",
            now + 60,
            "c811-isolated-acceptance",
            PublicRealm {
                issuer: native.issuer.clone(),
                company: COMPANY.into(),
                host: native.fx.host.clone(),
            },
            now,
        ),
        "second supported public session",
    )
    .expect("fresh second synthetic JTI");
    Session {
        cookie: format!("__Host-aos-board-session={}", opened.token),
        token: opened.token,
        key: String::new(),
    }
}

#[test]
fn cad811_missing_freeze_content_are_absent_only_under_live_native_authority() {
    // Native::new uses refusing providers, private state/PM/socket and a real
    // startup acknowledgement on owned 3110-3199 EXCLUDING 3139 and 3140.
    // The existing Standalone Fixture::start is deliberately NOT used.
    let native = Native::new_for_missing_state();
    let scope = native
        .fx
        .install("missing-state", &["audience.show", "content.show"]);
    let (owner, _) = native.owner();
    let other = second_owner(&native);
    // Prove the second session is live without consuming or superseding the
    // primary mount. The refusal below must test binding, not a fake cookie.
    must(
        native.rpc(
            "app_screen_mint",
            native.credentials(
                &other,
                "public",
                json!({"install_id": scope.install, "context_id": scope.context,
            "tag": "main", "generation": 1}),
            ),
        ),
        "second real session live",
    );
    let action = native.mount(&owner, "public", &scope, 1);
    assert!(
        native.lookups.load(Ordering::SeqCst) > 0,
        "fixture must exercise real Native membership proof"
    );

    // Make the proposal-only state real through Native RPC. It is honestly an
    // inert OPERATOR proposal; no assistant-origin attribution is claimed.
    let proposal_args = json!({"install_id": scope.install, "context_id": scope.context,
        "campaign_id": MISSING, "proposal_id": "pending-before-save",
        "subject": "Synthetic unapplied draft", "preheader": "",
        "blocks": [{"type": "paragraph", "text": "Synthetic pending body"}]});
    let proposal = must(
        native.rpc("app_content_propose", proposal_args),
        "real inert proposal setup",
    );
    assert!(
        proposal["proposal"]["state"] == "pending" && proposal["proposal"]["source_revision"] == 0,
        "proposal-only setup must not imply saved content"
    );
    let absent_counts = || {
        [
            count(
                &native,
                &scope,
                "SELECT count(*) FROM app_audience_freezes WHERE context_id=?1 AND freeze_id=?2",
                MISSING,
            ),
            count(
                &native,
                &scope,
                "SELECT count(*) FROM app_content_docs WHERE context_id=?1 AND campaign_id=?2",
                MISSING,
            ),
        ]
    };
    assert_eq!(
        absent_counts(),
        [0, 0],
        "missing rows must really be absent"
    );

    for id in [PRESENT, CORRUPT] {
        must(
            native.rpc(
                "app_content_save",
                json!({"install_id": scope.install,
            "context_id": scope.context, "campaign_id": id,
            "subject": "Saved synthetic control", "preheader": "",
            "blocks": [{"type": "paragraph", "text": "Synthetic saved body"}]}),
            ),
            "existing content setup",
        );
        must(
            native.rpc(
                "app_audience_prepare",
                json!({"install_id": scope.install,
            "context_id": scope.context, "freeze_id": id,
            "base": {"mode": "all"}, "max_recipients": 10}),
            ),
            "existing freeze setup",
        );
    }

    // Collect missing-outcome gaps rather than stopping at old untyped errors:
    // frozen old source must still exercise ALL authority/corruption controls.
    let mut gaps = Vec::new();
    let mut absence_codes = Vec::new();
    for operation in ["audience.show", "content.show"] {
        let expected = must(
            native.rpc(
                method(operation),
                native_args(&scope, selector(operation, PRESENT)),
            ),
            "native existing control",
        );
        let resource = if operation == "audience.show" {
            &expected["freeze"]
        } else {
            &expected["content"]
        };
        assert_eq!(resource["install_id"], scope.install);
        assert_eq!(resource["context_id"], scope.context);
        if operation == "audience.show" {
            assert_eq!(resource["freeze_id"], PRESENT);
            assert_eq!(expected["valid"], true);
        } else {
            assert_eq!(resource["campaign_id"], PRESENT);
            assert_eq!(resource["revision"], 1);
            assert_eq!(resource["subject"], "Saved synthetic control");
        }
        let body = call(&action, operation, selector(operation, PRESENT));
        let rpc = must(
            read_rpc(&native, &owner, body.clone()),
            "live existing RPC read",
        );
        assert_eq!(
            rpc, expected,
            "existing RPC must preserve real Native bytes"
        );
        let (status, reply) = native.fx.http("operator", &owner, &body);
        assert_eq!(status, 200, "existing HTTP must still succeed");
        assert_eq!(reply, expected, "existing HTTP must preserve Native result");
        let args = selector(operation, MISSING);
        let direct = error_only(
            native.rpc(method(operation), native_args(&scope, args.clone())),
            "genuinely absent native read",
        );
        assert_eq!(
            direct.kind(),
            "rejected",
            "absence must remain a controlled error"
        );
        let code = direct
            .code()
            .filter(|code| !code.is_empty())
            .map(str::to_owned);
        if code.is_none() {
            gaps.push(format!("{operation}:native absence unidentifiable"));
        }
        let body = call(&action, operation, args);
        let service = error_only(read_rpc(&native, &owner, body.clone()), "absent screen RPC");
        assert_eq!(service.kind(), "rejected");
        if service.code() != code.as_deref() {
            gaps.push(format!("{operation}:RPC lost Native absence identity"));
        }
        let (status, reply) = native.fx.http("operator", &owner, &body);
        if status != 404 {
            gaps.push(format!("{operation}:HTTP expected 404 got {status}"));
        }
        no_resource(&reply, "absent HTTP");
        absence_codes.push(code);
    }
    assert_eq!(
        absent_counts(),
        [0, 0],
        "missing reads must not manufacture rows"
    );

    for (operation, absence) in ["audience.show", "content.show"]
        .into_iter()
        .zip(absence_codes.iter().map(Option::as_deref))
    {
        let body = call(&action, operation, selector(operation, MISSING));
        denied(
            &native,
            &other,
            Asserted::Operator,
            body.clone(),
            absence,
            "wrong live session",
        );
        {
            let (agent_native, agent_owner, agent_body) = isolated_agent_probe(operation);
            denied(
                &agent_native,
                &agent_owner,
                Asserted::Agent("c811-missing-probe".into()),
                agent_body,
                absence,
                "agent holding valid owner credentials",
            );
        }
        let mut forged = body.clone();
        forged["args"]["context_id"] = json!("foreign-context");
        denied(
            &native,
            &owner,
            Asserted::Operator,
            forged,
            absence,
            "forged scope fields",
        );
        let malformed = call(&action, operation, selector(operation, "../malformed"));
        denied(
            &native,
            &owner,
            Asserted::Operator,
            malformed,
            absence,
            "malformed selector",
        );
        for context in [&scope.context[..], "unknown-context"] {
            let mut args = native_args(
                &scope,
                selector(
                    operation,
                    if context == scope.context {
                        "../malformed"
                    } else {
                        MISSING
                    },
                ),
            );
            args["context_id"] = json!(context);
            let error = error_only(
                native.rpc(method(operation), args),
                "native malformed/wrong scope",
            );
            assert!(matches!(error.kind(), "rejected" | "gate"));
            not_absence(&error, absence, "native malformed/wrong scope");
        }
    }

    // Corruption is injected only into TWO known rows of this owned record DB,
    // never core, permissions, sessions, membership, mounts or approval data.
    let path = must(
        crate::store::app_records::record_db_path(native.fx.root.path(), &scope.install),
        "owned installation record path",
    );
    assert!(
        path.starts_with(native.fx.root.path()),
        "no foreign database injection"
    );
    let db = must(
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE),
        "owned corruption fixture connection",
    );
    for sql in [
        "UPDATE app_audience_freezes SET base='not-json' WHERE context_id=?1 AND freeze_id=?2",
        "UPDATE app_content_docs SET subject=x'00' WHERE context_id=?1 AND campaign_id=?2",
    ] {
        assert_eq!(
            must(
                db.execute(sql, params![scope.context, CORRUPT]),
                "owned bounded storage corruption"
            ),
            1,
            "exactly one seeded row must be corrupted"
        );
    }
    drop(db);
    for (operation, absence) in ["audience.show", "content.show"]
        .into_iter()
        .zip(absence_codes.iter().map(Option::as_deref))
    {
        let direct = error_only(
            native.rpc(
                method(operation),
                native_args(&scope, selector(operation, CORRUPT)),
            ),
            "corrupt Native storage",
        );
        assert_eq!(
            direct.kind(),
            if operation == "audience.show" {
                "rejected"
            } else {
                "internal"
            },
            "corruption must reach the real storage error, not an access failure"
        );
        not_absence(&direct, absence, "corrupt Native storage");
        let body = call(&action, operation, selector(operation, CORRUPT));
        let service = error_only(
            read_rpc(&native, &owner, body.clone()),
            "corrupt screen storage",
        );
        assert_eq!(
            service.kind(),
            direct.kind(),
            "delegation must preserve storage failure"
        );
        not_absence(&service, absence, "corrupt screen storage");
        let (status, reply) = native.fx.http("operator", &owner, &body);
        assert!(
            matches!(status, 400 | 409 | 500),
            "corrupt storage must NOT be absent/success"
        );
        no_resource(&reply, "corrupt HTTP storage");
        if let Some(absence) = absence {
            assert_ne!(
                reply["code"].as_str(),
                Some(absence),
                "corruption acquired absence code"
            );
        }
    }

    let current = native.mount(&owner, "public", &scope, 2);
    for (operation, absence) in ["audience.show", "content.show"]
        .into_iter()
        .zip(absence_codes.iter().map(Option::as_deref))
    {
        denied(
            &native,
            &owner,
            Asserted::Operator,
            call(&action, operation, selector(operation, MISSING)),
            absence,
            "superseded mount",
        );
        // New generation still reads the uncorrupted control via BOTH peers.
        let body = call(&current, operation, selector(operation, PRESENT));
        must(
            read_rpc(&native, &owner, body.clone()),
            "new generation RPC live",
        );
        assert_eq!(
            native.fx.http("operator", &owner, &body).0,
            200,
            "new generation HTTP live before membership retirement"
        );
    }
    let checks = native.lookups.load(Ordering::SeqCst);
    native.member_live.store(false, Ordering::SeqCst);
    for (operation, absence) in ["audience.show", "content.show"]
        .into_iter()
        .zip(absence_codes.iter().map(Option::as_deref))
    {
        denied(
            &native,
            &owner,
            Asserted::Operator,
            call(&current, operation, selector(operation, MISSING)),
            absence,
            "retired Native membership",
        );
    }
    assert!(
        native.lookups.load(Ordering::SeqCst) > checks,
        "retirement must exercise the actual Native membership guard"
    );
    assert_eq!(
        absent_counts(),
        [0, 0],
        "refusals must leave both missing rows absent"
    );
    let after = must(
        native.rpc(
            "app_content_proposal_show",
            json!({"install_id": scope.install,
        "context_id": scope.context, "proposal_id": "pending-before-save"}),
        ),
        "proposal-only durable readback",
    );
    assert_eq!(
        after, proposal,
        "read/refusal paths must not apply/discard/change proposal"
    );
    assert!(gaps.is_empty(), "F1_MISSING_STATE_OUTCOME: {gaps:?}");
    eprintln!("{FILTER}: one case; real Native/socket/public HTTP; no send/provider calls");
}

const SECOND_OWNER: &str = "c811-missing-other-owner";

impl Native {
    fn new_for_missing_state() -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override; do not clear caller identity"
        );
        let root = must(
            tempfile::Builder::new().prefix("c811owner").tempdir(),
            "temporary state",
        );
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        must(crate::issue::Pm::init(&pm), "isolated PM");

        // Reserve the board through its REAL startup acknowledgement, not a
        // racy health check or adoption of a neighbor. Only owned 3110-3199.
        // Build Shared after selecting a disjoint free candidate; the actual
        // board bind still must acknowledge success or this attempt is BLOCKED.
        let reservation = crate::test_ports::reserve_board_port();
        let (port, reservation) = reservation;
        let host = format!("c811-owner.board.localhost:{port}");
        let issuer = format!("http://127.0.0.1:{port}");
        must(
            crate::board_identity::write_config(
                &state,
                &crate::board_identity::Config {
                    host: host.clone(),
                    issuer: issuer.clone(),
                    company: COMPANY.into(),
                },
            ),
            "synthetic board realm",
        );
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            lease: Some(Default::default()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().expect("fixture PM path"));
        // Exactly the supported Native fixture binding (not Standalone).
        // These markers belong only to this private ServeOptions, not the
        // caller environment, production org, installed binary or PM policy.
        for (name, value) in [
            ("AGENTICOS_COMPANY", COMPANY),
            ("AGENTICOS_INSTANCE", INSTANCE),
            ("AGENTICOS_BOARD_HOST", host.as_str()),
            ("AGENTICOS_BOARD_ISSUER", issuer.as_str()),
            ("AGENTICOS_BOARD_COMPANY", COMPANY),
        ] {
            opts.provider_env.set(name, value);
        }
        let mut shared = must(
            Shared::new(&state, &opts),
            "Native Shared construction/admission",
        );
        assert!(
            matches!(
                &shared.membership_mode,
                crate::daemon::public_membership::MembershipMode::Native(_)
            ),
            "fixture BLOCKED: expected Native, never Standalone"
        );
        let member_live = Arc::new(AtomicBool::new(true));
        let lookups = Arc::new(AtomicUsize::new(0));
        let (live, seen) = (member_live.clone(), lookups.clone());
        Arc::get_mut(&mut shared)
            .expect("fresh unique Shared")
            .membership_lookup_for_test = Some(Box::new(move |_hash, sub, company, instance| {
            assert!(
                [OWNER, SECOND_OWNER].contains(&sub) && company == COMPANY && instance == INSTANCE,
                "fixture BLOCKED: wrong synthetic membership binding"
            );
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(if live.load(Ordering::SeqCst) {
                Some(Role::Operator)
            } else {
                None
            })
        }));
        // A registered inert mailbox, NOT a running master/provider. No actor
        // loop is started, so a brief can persist but cannot execute a model.
        must(
            shared.store.register_agent(&crate::store::NewAgent {
                alias: "master",
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: state.to_str().expect("fixture state path"),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            }),
            "synthetic inert master registration",
        );

        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let listener = must(
            UnixListener::bind(state.join("cadence.sock")),
            "owned RPC bind",
        );
        must(listener.set_nonblocking(true), "RPC listener mode");
        let (stop, dispatch) = (servers.stop.clone(), shared.clone());
        servers.threads.push(std::thread::spawn(move || {
            let mut connections = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let dispatch = dispatch.clone();
                        connections.push(std::thread::spawn(move || {
                            serve::handle_conn(dispatch, stream)
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => panic!("owned RPC listener failed"),
                }
            }
            for connection in connections {
                assert!(connection.join().is_ok(), "owned RPC connection panicked");
            }
        }));
        let seam = Seam::token_at(&state).expect("fixture BLOCKED: native seam was not armed");
        let (ready, receipt) = std::sync::mpsc::channel();
        let board_opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(servers.stop.clone()),
            startup: Some(ready),
            test_seam: true,
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            public: Some(crate::ui::PublicBoard {
                host: host.clone(),
                authorize_url: format!("{issuer}/unused-authorize"),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            }),
            ..Default::default()
        };
        // A concurrent bind wins => BLOCKED, never kill/adopt it or change ports
        // outside the approved range. Servers RAII still joins our own socket.
        drop(reservation);
        servers.threads.push(std::thread::spawn(move || {
            drop(crate::ui::serve(&state, &pm, &board_opts))
        }));
        let bound = must(
            receipt.recv_timeout(Duration::from_secs(10)),
            "board startup receipt",
        );
        assert!(
            bound.is_ok(),
            "fixture BLOCKED: real board could not bind reserved port"
        );
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let fx = Fixture {
            servers,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host,
            seam,
        };
        Self {
            fx,
            shared,
            member_live,
            lookups,
            issuer,
        }
    }
}

fn isolated_agent_probe(operation: &str) -> (Native, Session, Value) {
    let native = Native::new_for_missing_state();
    must(
        native.shared.store.register_agent(&crate::store::NewAgent {
            alias: "c811-missing-probe",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: native.fx.root.path().to_str().expect("fixture state path"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        }),
        "ordinary inert refusal worker registration",
    );
    let scope = native.fx.install("agent-refusal", &[operation]);
    let (owner, _) = native.owner();
    let action = native.mount(&owner, "public", &scope, 1);
    let body = call(&action, operation, selector(operation, MISSING));
    (native, owner, body)
}
