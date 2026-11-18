//! CAD-2347 independent refusal acceptance for the member stream's agent-scope
//! retry. Authored from the ticket by a session other than the implementer's;
//! the product implementer must not edit or weaken it.
//!
//! The fix lets `MemberReader::open`/`filter` ask the daemon for the reader's
//! agent scope twice instead of once. The privacy risk is a retry that keeps a
//! reader connected, or feeds it frames, after the reader lost access. Every
//! test drives the real signed HTTP board stream (real daemon, real board, real
//! platform fixture) and asserts what the member's socket actually receives.
//!
//! What the seams allow, stated plainly:
//! * `agent_reads_ignore_relay` bit 1 is a LEVEL fault: while set, every
//!   `agent_list` the board relays comes back unscoped, which the board
//!   refuses. It cannot make one ask fail and the next succeed, so the retry's
//!   success path is only reached by the timed window in the last test, whose
//!   privacy assertions hold on every interleaving.
//! * `job_list` (read just before the scope ask) is not covered by that fault.
//!   `member_board_over` re-asks `agent_list` directly (no retry) and compares,
//!   so a scope refusal is also caught there, independent of `retry_once`.
//! * Session expiry has no seam; revocation is exercised through the real
//!   `POST /api/session/logout`.
//!
//! Bite results (temporary product breaks, reverted): only
//! `a_persistent_scope_refusal_closes_the_stream_and_refuses_to_open` fails
//! when `retry_once` retries until success. Turning a refused scope into an
//! empty successful one fails no test here (the board re-check above catches
//! it). Session revalidation failing open is caught only intermittently by
//! `a_revoked_session_closes_an_idle_stream`. The other tests are
//! layered-defence guards: they hold a real privacy property end to end but do
//! not isolate the retry.

use super::*;
use std::sync::atomic::AtomicU8;

type BatchHook = Arc<dyn Fn(&str, &[Arc<str>]) + Send + Sync>;

/// Mutable authority and fault levers of one rig.
struct Ctl {
    /// sub -> true when the platform still says operator, false when member.
    /// A missing sub is a withdrawn membership.
    roles: Arc<Mutex<BTreeMap<String, bool>>>,
    /// Bit 1 makes every relayed `agent_list` come back unscoped.
    scope_fault: Arc<AtomicU8>,
    /// The batch hook the board calls just before `reader.filter`.
    hook: Arc<Mutex<Option<BatchHook>>>,
}

const MEMBER: &str = "cad2347-member";
const CONTROL: &str = "cad2347-control";

fn rig() -> (Rig, Ctl) {
    block_sigchld();
    let dir = tempfile::Builder::new()
        .prefix("cad2347-http")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    for root in [&state, &pm] {
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let ctl = Ctl {
        roles: Arc::new(Mutex::new(BTreeMap::from([
            (MEMBER.to_string(), false),
            (CONTROL.to_string(), true),
        ]))),
        scope_fault: Arc::new(AtomicU8::new(0)),
        hook: Arc::new(Mutex::new(None)),
    };

    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound an unexpected address"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    {
        let (stop, authority, issuer, public_host) = (
            stop.clone(),
            authority.clone(),
            issuer.clone(),
            public_host.clone(),
        );
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                answer_platform(
                    request,
                    &authority,
                    &issuer,
                    &public_host,
                    &public_key,
                    &verify_public,
                );
            }
        }));
    }

    let roles = ctl.roles.clone();
    let lookup: crate::daemon::MembershipLookupForFixture =
        Arc::new(move |_hash, sub, company, instance| {
            assert_eq!(company, ORG);
            assert_eq!(instance, "abc1");
            Ok(roles.lock().unwrap().get(sub).map(|operator| {
                if *operator {
                    crate::board_identity::Role::Operator
                } else {
                    crate::board_identity::Role::Member
                }
            }))
        });
    let mode =
        crate::daemon::TestMembershipMode::Native(crate::daemon::TestNativeMembershipContext {
            company: ORG.into(),
            instance: "abc1".into(),
            realm: crate::operator_auth::PublicRealm {
                issuer: issuer.clone(),
                company: ORG.into(),
                host: public_host.clone(),
            },
        });
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            membership_mode_for_test: Some(mode),
            membership_lookup_for_test: Some(lookup),
            agent_reads_ignore_relay: Some(ctl.scope_fault.clone()),
            checkup: Some(0),
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "fixture daemon did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let slot = ctl.hook.clone();
    let hook: Arc<crate::ui::MemberStreamBatchHook> = Arc::new(move |actor, batch| {
        let current = slot.lock().unwrap().clone();
        if let Some(current) = current {
            current(actor, batch);
        }
    });
    let mut board_port = 0;
    for port in 3110..=3199u16 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            member_stream_batch_hook: Some(hook.clone()),
            ..ServeOpts::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "fixture board could not bind");
    let rig = Rig {
        _dir: dir,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        authority,
        stop,
        _threads: threads,
    };
    (rig, ctl)
}

/// The fixture the privacy checks share, with the secret project readable to
/// everyone, so a positive control proves the member really receives private
/// content before access is taken away.
fn open_world() -> (Rig, Ctl) {
    let (rig, ctl) = rig();
    seed(&rig);
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();
    (rig, ctl)
}

fn set_issue(rig: &Rig, project: &str, id: &str, title: &str) {
    std::fs::write(
        rig.pm.join(format!("{project}/{id}/issue.md")),
        issue_text(id, title),
    )
    .unwrap();
}

fn status_of(rig: &Rig, cookie: &str, method: &str, path: &str) -> u16 {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let origin = format!("http://{}", rig.public_host);
    let agent = http_agent();
    let response = if method == "POST" {
        agent
            .post(&url)
            .header("Content-Type", "application/json")
            .header("X-Cadence-Board", "1")
            .header("Host", &rig.public_host)
            .header("Origin", &origin)
            .header("Sec-Fetch-Site", "same-origin")
            .header("Cookie", cookie)
            .send_json(json!({}))
    } else {
        agent
            .get(&url)
            .header("Host", &rig.public_host)
            .header("Origin", &origin)
            .header("Sec-Fetch-Site", "same-origin")
            .header("Cookie", cookie)
            .call()
    };
    response.expect("board request failed").status().as_u16()
}

/// Read to an explicit EOF. A deadline is a failure, never a pass: a stream
/// that stays open past it is exactly what these checks exist to catch.
fn frames_until_closed(stream: &mut EventStream, seconds: u64, what: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut frames = String::new();
    while let Some(frame) = stream.next(deadline, what) {
        frames.push_str(&frame);
    }
    frames
}

fn no_content(frames: &str, markers: &[&str], who: &str) {
    for marker in markers {
        assert!(
            !frames.contains(marker),
            "{who} received {marker} after losing access; frames: {frames}"
        );
    }
}

/// Layered-defence guard: the withdrawn reader is refused by the session and
/// read checks before the scope retry matters; no retry bite fails it.
#[test]
fn withdrawn_membership_gets_no_further_frame_and_the_stream_closes() {
    let (rig, ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");

    // Positive control: before the withdrawal the member does receive private
    // content, so a later silence cannot be a stream that never carried it.
    set_issue(&rig, "secret", "SEC-1", "CAD2347_MEMBERSHIP_BEFORE");
    control_stream.through("CAD2347_MEMBERSHIP_BEFORE");
    assert!(member_stream
        .through("CAD2347_MEMBERSHIP_BEFORE")
        .contains("SEC-1"));

    ctl.roles.lock().unwrap().remove(MEMBER);
    assert_ne!(
        status_of(&rig, &member, "GET", "/api/projects"),
        200,
        "withdrawn membership still read through the same session"
    );

    set_issue(&rig, "secret", "SEC-1", "CAD2347_MEMBERSHIP_AFTER_PRIVATE");
    control_stream.through("CAD2347_MEMBERSHIP_AFTER_PRIVATE");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_MEMBERSHIP_AFTER_PUBLIC");
    control_stream.through("CAD2347_MEMBERSHIP_AFTER_PUBLIC");

    let frames = frames_until_closed(&mut member_stream, 25, "withdrawn member close");
    no_content(
        &frames,
        &[
            "CAD2347_MEMBERSHIP_AFTER_PRIVATE",
            "CAD2347_MEMBERSHIP_AFTER_PUBLIC",
        ],
        "withdrawn member",
    );
}

/// Layered-defence guard: inside the filter `job_list` refuses the withdrawn
/// reader before the scope is asked, so this does not isolate the retry.
#[test]
fn membership_withdrawn_between_the_last_guard_and_the_filter_delivers_nothing() {
    let (rig, ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");

    set_issue(&rig, "secret", "SEC-1", "CAD2347_WINDOW_BEFORE");
    control_stream.through("CAD2347_WINDOW_BEFORE");
    member_stream.through("CAD2347_WINDOW_BEFORE");

    // The batch hook runs after the stream's last liveness guard and before
    // `reader.filter`: the exact window in which the scope is (re)asked. The
    // membership is withdrawn inside it, so both asks must be refused.
    let roles = ctl.roles.clone();
    let fired = Arc::new(AtomicBool::new(false));
    let fired_in_hook = fired.clone();
    *ctl.hook.lock().unwrap() = Some(Arc::new(move |_actor, batch| {
        if batch.iter().any(|f| f.contains("CAD2347_WINDOW_PRIVATE"))
            && !fired_in_hook.swap(true, Ordering::SeqCst)
        {
            roles.lock().unwrap().remove(MEMBER);
        }
    }));
    set_issue(&rig, "secret", "SEC-1", "CAD2347_WINDOW_PRIVATE");
    control_stream.through("CAD2347_WINDOW_PRIVATE");

    let frames = frames_until_closed(&mut member_stream, 25, "window member close");
    assert!(
        fired.load(Ordering::SeqCst),
        "the member never reached the filter window"
    );
    no_content(
        &frames,
        &["CAD2347_WINDOW_PRIVATE", "SEC-1"],
        "member whose membership was withdrawn in the window",
    );
}

/// Layered-defence guard: project policy hides the work whatever the scope
/// answer; no retry bite fails it.
#[test]
fn lost_project_policy_hides_private_work_from_an_open_stream() {
    let (rig, _ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");

    set_issue(&rig, "secret", "SEC-1", "CAD2347_POLICY_BEFORE");
    control_stream.through("CAD2347_POLICY_BEFORE");
    member_stream.through("CAD2347_POLICY_BEFORE");

    // The project stops admitting the member: private visibility, no grant.
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    set_issue(&rig, "secret", "SEC-1", "CAD2347_POLICY_AFTER_PRIVATE");
    control_stream.through("CAD2347_POLICY_AFTER_PRIVATE");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_POLICY_CHECKPOINT");
    control_stream.through("CAD2347_POLICY_CHECKPOINT");

    let frames = member_stream.until_checkpoint_or_close("CAD2347_POLICY_CHECKPOINT");
    no_content(
        &frames,
        &["CAD2347_POLICY_AFTER_PRIVATE", "SEC-1"],
        "member of a project that stopped admitting it",
    );
}

/// Layered-defence guard: the lost job is dropped by the reader's job state
/// (`job_list`) before the scope ask, so this does not isolate the retry.
#[test]
fn lost_job_access_delivers_no_later_frame_for_that_job() {
    let (rig, _ctl) = rig();
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let running = |id: &str, title: &str| {
        shared
            .store
            .enqueue_task(
                "cad2215-worker",
                title,
                None,
                id,
                "user",
                Some("cad2215-visible-task"),
            )
            .unwrap();
        assert_eq!(
            db.execute("UPDATE messages SET state='running' WHERE id=?1", [id])
                .unwrap(),
            1
        );
    };
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");

    // Positive control: the member's job is readable, so its running message
    // reaches the member before access is taken away.
    running("cad2347-job-before", "CAD2347_JOB_BEFORE");
    control_stream.through("CAD2347_JOB_BEFORE");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_JOB_CHECKPOINT_1");
    control_stream.through("CAD2347_JOB_CHECKPOINT_1");
    assert!(
        member_stream
            .through("CAD2347_JOB_CHECKPOINT_1")
            .contains("cad2347-job-before"),
        "member did not receive the readable running message"
    );

    // The job moves onto the private project's issue: the member's access to
    // it is gone. The same-session HTTP read proves the revocation took effect.
    assert_eq!(
        db.execute(
            "UPDATE jobs SET issue_id='SEC-1' WHERE id='cad2215-visible-job'",
            []
        )
        .unwrap(),
        1
    );
    let after = get(&rig, &member, "/api/agents").to_string();
    assert!(
        !after.contains("cad2215-visible-task") && !after.contains("cad2347-job-before"),
        "member still reads the job it lost: {after}"
    );
    assert!(
        get(&rig, &control, "/api/agents")
            .to_string()
            .contains("cad2215-visible-task"),
        "control lost the job"
    );

    running("cad2347-job-after", "CAD2347_JOB_AFTER");
    control_stream.through("CAD2347_JOB_AFTER");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_JOB_CHECKPOINT_2");
    control_stream.through("CAD2347_JOB_CHECKPOINT_2");
    let frames = member_stream.until_checkpoint_or_close("CAD2347_JOB_CHECKPOINT_2");
    no_content(
        &frames,
        &["cad2347-job-after", "CAD2347_JOB_AFTER"],
        "member that lost the job",
    );
}

#[test]
fn a_revoked_session_closes_an_idle_stream() {
    let (rig, _ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");

    // Revoke through the real route, then write nothing. Layered-defence
    // guard, not an isolation of the session guard: the watcher still emits
    // batches of its own (a `monitoring` batch after logout was observed), and
    // the filter's daemon reads refuse the dead token and close the stream
    // too. With session revalidation made to fail open this test failed in
    // only 1 of 9 runs.
    assert_eq!(
        status_of(&rig, &member, "POST", "/api/session/logout"),
        204,
        "logout was not accepted"
    );
    assert_ne!(
        status_of(&rig, &member, "GET", "/api/projects"),
        200,
        "revoked session still reads"
    );
    let frames = frames_until_closed(&mut member_stream, 20, "revoked session close");
    no_content(&frames, &["VIS-1", "SEC-1"], "revoked session");

    // The board itself is alive and a still-valid reader is unaffected.
    set_issue(&rig, "visible", "VIS-1", "CAD2347_SESSION_CHECKPOINT");
    control_stream.through("CAD2347_SESSION_CHECKPOINT");
}

/// Bites a retry that never gives up (the open hangs instead of answering
/// 503). A refusal turned into a successful empty scope is still caught by
/// `member_board_over`'s own scope re-check, so this does not catch that.
#[test]
fn a_persistent_scope_refusal_closes_the_stream_and_refuses_to_open() {
    let (rig, ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");

    // At open: a scope that cannot be proven is a truthful 503, not a stream.
    ctl.scope_fault.store(1, Ordering::SeqCst);
    let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(25)))
        .unwrap();
    write!(
        socket,
        "GET /api/stream?entities=1 HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
        rig.public_host, member
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    assert!(
        line.starts_with("HTTP/1.1 503 "),
        "stream opened on an unproven scope: {line}"
    );

    // Mid-stream: the same persistent refusal closes an open stream before the
    // next frame, however many times the scope is asked.
    ctl.scope_fault.store(0, Ordering::SeqCst);
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let mut member_stream = EventStream::open(&rig, &member, "member");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_FAULT_BEFORE");
    control_stream.through("CAD2347_FAULT_BEFORE");
    member_stream.through("CAD2347_FAULT_BEFORE");
    ctl.scope_fault.store(1, Ordering::SeqCst);
    set_issue(&rig, "visible", "VIS-1", "CAD2347_FAULT_AFTER");
    control_stream.through("CAD2347_FAULT_AFTER");
    let frames = frames_until_closed(&mut member_stream, 25, "persistent refusal close");
    no_content(
        &frames,
        &["CAD2347_FAULT_AFTER"],
        "reader with no provable scope",
    );

    // And it was the fault, not the member: with it cleared a fresh stream works.
    ctl.scope_fault.store(0, Ordering::SeqCst);
    let mut fresh = EventStream::open(&rig, &member, "fresh member");
    set_issue(&rig, "visible", "VIS-1", "CAD2347_FAULT_RECOVERED");
    fresh.through("CAD2347_FAULT_RECOVERED");
}

/// Layered-defence guard over the retry's success path: project policy and
/// the board re-check keep the private work out on every interleaving; no
/// retry bite fails it.
#[test]
fn a_refused_first_ask_never_lets_pre_revocation_content_through() {
    // For each delay the scope fault is raised inside the filter window and
    // lowered `delay` later, so across rounds the first ask is refused and the
    // second sometimes answers, sometimes also refuses. Access is revoked in
    // the same window. Whatever the interleaving, the member must never be
    // handed content decided before the revocation.
    let (rig, ctl) = open_world();
    let member = login(&rig, MEMBER, "member");
    let control = login(&rig, CONTROL, "owner");
    let mut control_stream = EventStream::open(&rig, &control, "control");
    let (mut survived, mut closed) = (0, 0);
    for (round, delay_ms) in [0u64, 1, 2, 3, 5, 8, 13, 21, 34, 55, 400]
        .into_iter()
        .enumerate()
    {
        let private = format!("CAD2347_ROUND_{round}_PRIVATE");
        let public = format!("CAD2347_ROUND_{round}_PUBLIC");
        let open_policy = "key: secret\nprefix: SEC\n";
        std::fs::write(rig.pm.join("secret/project.yaml"), open_policy).unwrap();
        ctl.scope_fault.store(0, Ordering::SeqCst);
        let mut member_stream = EventStream::open(&rig, &member, "round member");

        let releaser: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::default();
        let (fault, policy) = (ctl.scope_fault.clone(), rig.pm.join("secret/project.yaml"));
        let armed = Arc::new(AtomicBool::new(true));
        let (armed_in_hook, releaser_in_hook, marker) =
            (armed.clone(), releaser.clone(), private.clone());
        *ctl.hook.lock().unwrap() = Some(Arc::new(move |_actor, batch| {
            if batch.iter().any(|f| f.contains(marker.as_str()))
                && armed_in_hook.swap(false, Ordering::SeqCst)
            {
                std::fs::write(
                    &policy,
                    "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
                )
                .unwrap();
                fault.store(1, Ordering::SeqCst);
                let fault = fault.clone();
                *releaser_in_hook.lock().unwrap() = Some(std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    fault.store(0, Ordering::SeqCst);
                }));
            }
        }));
        set_issue(&rig, "secret", "SEC-1", &private);
        control_stream.through(&private);
        set_issue(&rig, "visible", "VIS-1", &public);
        control_stream.through(&public);
        let frames = member_stream.until_checkpoint_or_close(&public);
        if let Some(handle) = releaser.lock().unwrap().take() {
            handle.join().unwrap();
        }
        ctl.scope_fault.store(0, Ordering::SeqCst);
        *ctl.hook.lock().unwrap() = None;
        assert!(
            !armed.load(Ordering::SeqCst),
            "round {round}: the member never reached the filter window"
        );
        no_content(
            &frames,
            &[private.as_str(), "SEC-1"],
            &format!("member in retry round {round} (delay {delay_ms}ms)"),
        );
        if frames.contains(&public) {
            survived += 1;
        } else {
            closed += 1;
        }
    }
    eprintln!("cad2347 retry window rounds: survived={survived} closed={closed}");
}
