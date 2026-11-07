//! CAD-2207 — a SELECTED receipt-linked notice must not keep arriving on a
//! subscriber's history RESPONSE after the owner's local unshare already
//! succeeded.
//!
//! Independently authored negative acceptance, deliberately separate from the
//! implementation and from every other `team_assistants_refusal` check. This
//! module is a CHILD of that module only to reuse its already-real fixture
//! (`Fx`: a real in-process daemon, a real `operator/board-identity.json` trust
//! root and one loopback authoritative platform answering
//! `/__platform/assistant/revalidate`) plus the real `Store`. Every value under
//! test is produced by the REAL code path: the owner's `member_job_request`,
//! the real `member_job_share`/`member_job_subscribe` RPCs on signed member
//! envelopes, the real `assist_request_notice` + `route_assist_job_notifications`
//! intent drain that enqueues the delivered notice into the bound assistant's
//! own thread, and the REAL `Shared::dispatch` reached through the production
//! `crate::daemon::serve::handle_conn` Unix-socket path. No authorization guard
//! is mirrored and no response is fabricated.
//!
//! THE WINDOW UNDER TEST. `handle_conn` resolves the whole reply with
//! `Shared::dispatch` and only then frames it:
//!
//! ```text
//! dispatch("member_assist_history") -> Value   // notice_visible_sql ran here
//! frame = proto::ok(value) + fleet_protocol
//! writeln!(BufWriter<UnixStream>, "{frame}")   // bytes leave here, with no
//! writer.flush()                                // further share check
//! ```
//!
//! `rpc_member_assist_history` selects its rows with
//! `src/store/threads.rs::notice_visible_sql`, which reads the CURRENT local
//! share at query time. A notice selected while the share was live therefore
//! sits inside `Value` when the owner's `member_job_share {enabled:false}`
//! completes. If the response is still being emitted (a large page, a peer
//! draining slowly), the unshare has SUCCEEDED while the already-selected
//! notice continues out of the socket. That is the silent leak this test
//! fails on.
//!
//! DETERMINISM / NO SLEEPS.
//!   * the socket daemon is a child seam-armed `Shared` over the SAME real
//!     platform, authority state and signing key as the fixture: `Fx::new`
//!     builds `ServeOptions::default()`, whose `test_seam` is false (the env
//!     only arms `serve()`), and `handle_conn` honors `test_caller` only on a
//!     seam-armed daemon. The child seam credential is minted in its own temp
//!     state dir and authorizes exactly the fixture's own caller;
//!   * a `UnixStream::pair()` carries the REAL `handle_conn`; the daemon end's
//!     `SO_SNDBUF` and the client end's `SO_RCVBUF` are pinned small, and the
//!     page is sized (from the ACTUAL effective buffer values and the in-process
//!     control response length) larger than both buffers plus the prefix read;
//!     `done` is asserted false after the prefix, proving the reply is still in
//!     flight and not merely slow to compute;
//!   * the subscriber's thread is seeded with ordinary filler entries BEFORE the
//!     canonical job, so the unique private notice is the LAST entry and cannot
//!     appear in the initially read prefix (asserted);
//!   * the test waits for that prefix, then issues the REAL signed
//!     `member_job_share {enabled:false}` on its own bounded thread while the
//!     response is held, and does NOT drain the response until the withdrawal
//!     verdict is known;
//!   * one UNRELATED shared job is withdrawn in the same window and must
//!     succeed: a fix may reserve only the racing job, never serialize every
//!     share against one response.
//!
//! ACCEPTED OUTCOMES. If the owner's unshare for the racing job reports
//! `shared:false` (and the share row is actually gone), the full socket
//! response must be an explicit coded refusal or must not carry the notice —
//! its id, its delivered body, or the assistant-origin source marker. A
//! `share_withdrawal_pending` refusal under stable code is accepted ONLY while
//! the share row is still live, and then the response is required to carry the
//! still-authorized notice. Any other error, a silent truncation, or a leaked
//! notice fails.
//!
//! BASELINE (at `eee39501`): nothing holds an admission across the response
//! write. The unshare completes while the socket write is blocked, and the
//! drained response still contains the delivered notice — so
//! `cad2207_blocked_history_response_cannot_outlive_a_completed_share_withdrawal`
//! fails on its withheld-notice assertions. It passes only when the local
//! decision that selected the notice survives through the bounded full socket
//! emission (a held per-job admission making the withdrawal refuse, or an
//! equivalent re-check before the bytes leave).
//!
//! SECOND CONTROL (added after the primary one, independently authored): the
//! self `thread_read` `reply_seq` watermark is derived from the WHOLE thread by
//! `Store::thread_reply_seq`, not from the requested page. A revoked notice
//! reply that is outside the requested page must therefore not cross the socket
//! as `reply_seq`. The control seeds an older VISIBLE agent reply and a newer
//! agent reply linked to the notice (the same shapes the actor writes), reads
//! the watermark through the REAL socket as the bound assistant, unshares, and
//! then asks for a page that STARTS PAST the notice reply: `reply_seq` must be
//! the older visible reply and the response must name neither the notice id nor
//! its body. It is a control (the watermark filter already exists at this head),
//! not a baseline-red case.
//!
//! THIRD CASE (in-flight reply watermark, also baseline-red): a blocked
//! `thread_read` asks for a page that STARTS PAST the notice reply, so the
//! revoked row is outside the page, while `result.reply_seq` was computed from
//! the whole thread while the share was live. Once the owner's unshare
//! COMPLETES, that stale watermark must not cross the socket (or the withdrawal
//! must be an explicit `share_withdrawal_pending` refusal with the share still
//! live). At this head the withdrawal succeeds and the blocked response still
//! carries the notice reply's seq, so
//! `cad2207_blocked_reply_watermark_cannot_outlive_a_completed_withdrawal`
//! fails on `result.reply_seq`.
//!
//! FOURTH CASE (transport, control): a history response that carries a live
//! receipt-linked notice takes `handle_conn`'s guarded NONBLOCKING bounded write
//! and must restore blocking mode before the next request. One real connection
//! carries a signed `member_assist_history` (fully read, client NOT
//! half-closed) followed by a signed `member_assist_state` on the SAME socket;
//! the second request must receive its complete frame. A missing
//! `set_nonblocking(false)` makes the daemon's next nonblocking read return
//! `WouldBlock` and close the connection, so this control fails.
//!
//! NOT COVERED (reported, never simulated): remote membership/session/
//! credential/grant revocation (owned by `member_authority_live`); the
//! continuity-pack prompt path; a provider request already sent; a copy a peer
//! already read; and generic `thread_read` pages that intentionally carry the
//! notice while the share is still live.
//!
//! Register (implementer edit, NOT made by this file):
//! `mod cad2207_history_socket_withdrawal_acceptance;` beside the other
//! `cad2207_*` children of `team_assistants_refusal.rs`.
//!
//! Run just this module's two cases after registration:
//! `scripts/boat-build -- cargo test --locked --features test-seam --lib
//! cad2207_history_socket_withdrawal -- --test-threads 1`

#![cfg(all(test, unix, feature = "test-seam"))]

use super::*;

use crate::daemon::{ServeOptions, Shared};
use crate::test_seam::Seam;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

/// The member who owns the canonical job and grants/withdraws its share.
const CAD2207_SOCKET_OWNER: &str = "sub-alice";
/// The second member whose bound assistant receives the delivered notice.
const CAD2207_SOCKET_SUBSCRIBER: &str = "sub-ben";
/// The racing job's canonical request key (the delivered notice names the job
/// id derived from it, so the secret exists nowhere else).
const CAD2207_SOCKET_REQUEST_KEY: &str = "cad2207-history-socket-race";
/// An UNRELATED job the same owner shares; its withdrawal must still complete
/// while the history response is in flight.
const CAD2207_SOCKET_UNRELATED_KEY: &str = "cad2207-history-socket-unrelated";
/// The in-flight `thread_read` reply-watermark race's canonical request key.
const CAD2207_SOCKET_REPLY_RACE_KEY: &str = "cad2207-history-socket-reply-race";
/// The sequential-connection acceptance's canonical request key.
const CAD2207_SOCKET_SEQUENTIAL_KEY: &str = "cad2207-history-socket-sequential";
/// The assistant-origin marker a delivered job notice carries.
const CAD2207_SOCKET_NOTICE_SOURCE: &str = "assist_job_event";
/// Every filler entry starts with this marker, so the test can prove the page
/// is the real (full) history without naming any notice.
const CAD2207_SOCKET_FILLER: &str = "cad2207-history-socket-filler-";
/// Bytes read from the socket before the withdrawal is issued. Far smaller than
/// the page and far smaller than the message id/body near its end.
const CAD2207_SOCKET_PREFIX: usize = 512;
/// One bounded liveness guard for a withdrawal while a response is blocked.
const CAD2207_SOCKET_WITHDRAWAL: Duration = Duration::from_secs(20);

/// A seam-armed daemon that REUSES the real fixture's platform, authority
/// state and Ed25519 key, but runs its own `Shared` over a child state dir with
/// `test_seam: true`. `ServeOptions::default()` leaves `test_seam` false (the
/// env only arms `serve()`), and `handle_conn` honors a frame's `test_caller`
/// only on a seam-armed daemon — so this is the smallest real fixture that can
/// dispatch the signed member relay on a socket thread. The seam credential is
/// minted in the child state dir; the platform/authority answers exactly as
/// they do for every other `Fx` test. Returns the fixture and the state dir
/// whose `seam/token` is the credential.
fn cad2207_socket_fx() -> (Fx, PathBuf) {
    let mut fx = Fx::new();
    let state = fx.dir.path().join("socket-state");
    std::fs::create_dir_all(&state).unwrap();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    // The same trust root and platform the base fixture uses: the member
    // envelopes this test mints and the live pulls it makes resolve there.
    crate::board_identity::write_config(
        &state,
        &crate::board_identity::Config {
            host: fx.host.clone(),
            issuer: fx.issuer.clone(),
            company: ORG.to_string(),
        },
    )
    .unwrap();
    let opts = ServeOptions {
        test_seam: true,
        ..ServeOptions::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(&state, &opts).unwrap();
    assert!(
        Seam::token_at(&state).is_some(),
        "the child fixture state dir was not seam-armed"
    );
    fx.shared = shared;
    // The routing pass authorizes a SUBSCRIBER's notice with
    // `member_authority_live("", ...)` — an EMPTY member envelope. That path is
    // only answerable by the daemon's own `hosted-board` `assist.revalidate`
    // service credential; without it the platform refuses and the subscriber
    // intent settles `refused` (no notice ever reaches the bound thread). The
    // sibling fixtures call `with_deferred_service`, which writes custody into
    // `Fx`'s BASE dir; this fixture must write the same two artifacts into the
    // CHILD state dir its `Shared` actually reads.
    cad2207_socket_enroll_service(&fx, &state);
    (fx, state)
}

/// Enroll the real `assist.revalidate` service credential into the CHILD state
/// dir's custody + store and the fixture platform's authority table. Mirrors
/// `Fx::enroll_service` exactly, only with `state` as the custody root.
fn cad2207_socket_enroll_service(fx: &Fx, state: &Path) {
    let service = ServiceAuthority {
        token: format!("hcs_{}", "d".repeat(43)),
        org: ORG.to_string(),
        audience: format!("http://{}", fx.host),
        caps: vec!["assist.revalidate".into()],
        live: true,
    };
    fx.authority
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .services
        .push(service.clone());
    let key = crate::platform::custody::Key {
        platform: "hosted-board",
        account: &fx.host,
    };
    crate::platform::custody::Custody::open(state)
        .unwrap()
        .put(&key, service.token.as_bytes())
        .unwrap();
    let fingerprint = crate::secret::fingerprint(service.token.as_bytes());
    let host = fx.host.clone();
    fx.shared
        .store
        .fixture_write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO platform_credentials
                 (platform,account,scopes,fingerprint,custody,exchange,enrolled_at,by,connection_id,credential_revision)
                 VALUES('hosted-board',?1,'[\"assist.revalidate\"]',?2,'file','token',1.0,'operator','conn-hosted-board',1)",
                rusqlite::params![host, fingerprint],
            )?;
            Ok(())
        })
        .unwrap();
}

/// The seam credential for `test_caller`. Read from the fixture state dir that
/// minted it; absent here is a fixture failure, never a pass.
fn cad2207_socket_token(state: &Path) -> String {
    Seam::token_at(state).unwrap_or_else(|| {
        panic!("the fixture minted no seam token — cannot assert the socket caller")
    })
}

/// The frame `handle_conn` reads: one request object plus the seam assertion
/// beside `method`/`params` (never inside `params`, so no handler can mistake
/// it for a request field).
fn cad2207_socket_frame(method: &str, params: Value, who: &str, token: &str) -> Vec<u8> {
    let mut frame = crate::proto::request(method, params);
    frame["test_caller"] = json!({"token": token, "as": who});
    let mut bytes = serde_json::to_vec(&frame).expect("socket request serializes");
    bytes.push(b'\n');
    bytes
}

/// Pin one socket buffer option; a failure here is a fixture error, not a pass.
fn cad2207_socket_set_buffer(stream: &UnixStream, option: libc::c_int, bytes: libc::c_int) {
    let rc = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            &bytes as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(
        rc,
        0,
        "the fixture could not pin socket option {option}: {}",
        std::io::Error::last_os_error()
    );
}

/// The ACTUAL effective buffer size (the kernel may round/double it).
fn cad2207_socket_buffer(stream: &UnixStream, option: libc::c_int) -> libc::c_int {
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            option,
            &mut value as *mut libc::c_int as *mut libc::c_void,
            &mut len,
        )
    };
    assert_eq!(
        rc,
        0,
        "the fixture could not read socket option {option}: {}",
        std::io::Error::last_os_error()
    );
    value
}

/// The delivered `assist_job_event` notice for ONE job on ONE alias — found by
/// the durable `assist_notice_deliveries` receipt, never by body text.
fn cad2207_socket_notice(fx: &Fx, alias: &str, job: &str) -> crate::store::Message {
    fx.shared
        .store
        .messages(alias)
        .unwrap_or_else(|error| panic!("could not read '{alias}' messages: {error}"))
        .into_iter()
        .find(|message| {
            message.source == CAD2207_SOCKET_NOTICE_SOURCE
                && fx
                    .shared
                    .store
                    .assist_notice_delivery(&message.id)
                    .unwrap()
                    .is_some_and(|delivery| delivery.job == job)
        })
        .unwrap_or_else(|| panic!("no delivered assist notice for {job} on '{alias}'"))
}

/// A bounded verdict for one REAL signed `member_job_share {enabled:false}`.
/// The call runs on its own thread so it is genuinely concurrent with the
/// blocked response and its own live-authority pull; the bound is a liveness
/// guard only. On expiry the caller drains the response before failing, so the
/// fixture can never hang and the diagnostic is not masking a refused write.
struct Cad2207Withdrawal {
    code: Option<String>,
    message: String,
    timed_out: bool,
}

fn cad2207_withdraw_bounded(fx: &Fx, job: &str) -> std::result::Result<Value, Cad2207Withdrawal> {
    let (sender, receiver) = mpsc::channel();
    let shared = fx.shared.clone();
    let envelope = fx.member_envelope(CAD2207_SOCKET_OWNER);
    let job = job.to_string();
    std::thread::spawn(move || {
        let answer = scoped(Asserted::Operator, || {
            shared.dispatch(
                "member_job_share",
                &json!({"member_envelope": envelope, "job": job, "enabled": false}),
                std::process::id(),
            )
        });
        let wire = answer.map_err(|error| (error.code().map(str::to_string), error.to_string()));
        let _ = sender.send(wire);
    });
    match receiver.recv_timeout(CAD2207_SOCKET_WITHDRAWAL) {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err((code, message))) => Err(Cad2207Withdrawal {
            code,
            message,
            timed_out: false,
        }),
        Err(_) => Err(Cad2207Withdrawal {
            code: None,
            message: "the withdrawal neither completed nor refused within the bound".to_string(),
            timed_out: true,
        }),
    }
}

/// The real owner request + REAL share + the subscriber's REAL subscription +
/// ONE delivered notice in the subscriber's bound thread from the real intent
/// drain. Returns the canonical job id and the delivered message.
fn cad2207_socket_share_and_notice(
    fx: &Fx,
    subscriber_alias: &str,
    request_key: &str,
    title: &str,
) -> (String, crate::store::Message) {
    let request = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_OWNER),
            "title": title,
            "request_key": request_key,
            "body": "cad2207 history socket: the owner's private job spec",
            "acceptance": "a blocked history response cannot outlive a completed withdrawal",
        }),
    );
    let job = request
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no job: {request}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_OWNER),
            "job": job,
            "enabled": true,
        }),
    );
    fx.op(
        "member_job_subscribe",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_SUBSCRIBER),
            "job": job,
        }),
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the fixture's owner share did not land"
    );
    assert!(
        fx.shared
            .store
            .job_is_subscriber(&job, ORG, CAD2207_SOCKET_SUBSCRIBER)
            .unwrap(),
        "the fixture did not create the durable subscriber row"
    );
    // The REAL intent drain writes the notice (and its delivery receipt) into
    // the bound threads. The body is the router's, never hand-written here.
    fx.shared
        .store
        .assist_request_notice(&job)
        .unwrap_or_else(|error| panic!("the assist notice intent did not commit: {error}"));
    fx.shared
        .route_assist_job_notifications()
        .unwrap_or_else(|error| panic!("the assist intent drain failed: {error}"));
    let notice = cad2207_socket_notice(fx, subscriber_alias, &job);
    assert!(
        notice.body.contains(job.as_str()),
        "the delivered notice body does not name the job: {}",
        notice.body
    );
    (job, notice)
}

/// The state one race fixture seeds.
struct Cad2207SocketHistory {
    job: String,
    unrelated_job: String,
    notice: crate::store::Message,
}

/// Two provisioned members, the owner's UNRELATED shared job, the subscriber's
/// ordinary filler thread, then the owner's canonical job + share + the
/// subscriber's subscription and ONE delivered notice. The filler entries are
/// seeded BEFORE the notice so the unique private notice is the LAST entry of
/// the page and lies beyond the prefix this test reads.
fn cad2207_socket_history_fixture(
    fx: &Fx,
    filler_count: usize,
    filler_body: usize,
) -> Cad2207SocketHistory {
    let _owner = fx.member_runtime(CAD2207_SOCKET_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SOCKET_SUBSCRIBER);

    // One UNRELATED shared job: its withdrawal must still succeed while the
    // racing job's history response is blocked. Nothing is subscribed to it,
    // so it cannot put a second notice in the subscriber's thread.
    let unrelated = fx.op(
        "member_job_request",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_OWNER),
            "title": "cad2207 history socket: an unrelated private job",
            "request_key": CAD2207_SOCKET_UNRELATED_KEY,
            "body": "cad2207 history socket: unrelated job body",
        }),
    );
    let unrelated_job = unrelated
        .get("job")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("member_job_request returned no unrelated job: {unrelated}"))
        .to_string();
    fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_OWNER),
            "job": unrelated_job,
            "enabled": true,
        }),
    );
    assert!(
        fx.shared
            .store
            .assist_job_is_shared(&unrelated_job, ORG)
            .unwrap(),
        "the fixture's unrelated share did not land"
    );

    // Ordinary, authorized thread content BEFORE the notice. These are plain
    // store enqueues (fixture content, not the notice): the guard under test is
    // the real selection predicate, and the notice itself is produced by the
    // real intent drain below.
    for index in 0..filler_count {
        let body = format!("{CAD2207_SOCKET_FILLER}{index}:{}", "x".repeat(filler_body));
        let id = format!("cad2207-history-socket-filler-{index}");
        fx.shared
            .store
            .enqueue(&subscriber.alias, &body, None, &id, "test")
            .unwrap_or_else(|error| panic!("could not seed filler entry {index}: {error}"));
    }

    let (job, notice) = cad2207_socket_share_and_notice(
        fx,
        &subscriber.alias,
        CAD2207_SOCKET_REQUEST_KEY,
        "cad2207 history socket: the racing private job",
    );

    // The unique private notice really is the newest entry: nothing after it in
    // this fixture. A response prefix therefore cannot legitimately contain it.
    assert!(
        notice.body.contains(job.as_str()),
        "the delivered notice lost its job identifier"
    );

    Cad2207SocketHistory {
        job,
        unrelated_job,
        notice,
    }
}

/// Read exactly `wanted` bytes or fail with the bytes actually seen, so an
/// early refusal is reported as such instead of a bare I/O error.
fn cad2207_read_prefix(client: &mut UnixStream, wanted: usize, what: &str) -> Vec<u8> {
    let mut prefix = Vec::with_capacity(wanted);
    let mut buf = [0u8; 4096];
    while prefix.len() < wanted {
        let take = (wanted - prefix.len()).min(buf.len());
        match client.read(&mut buf[..take]) {
            Ok(0) => break,
            Ok(read) => prefix.extend_from_slice(&buf[..read]),
            Err(error) => panic!("{what}: reading the response prefix failed: {error}"),
        }
    }
    assert!(
        prefix.len() >= wanted,
        "{what}: the socket response ended after {} bytes before the expected \
         prefix — the daemon likely refused the frame: {}",
        prefix.len(),
        String::from_utf8_lossy(&prefix)
    );
    prefix
}

/// ONE real `thread_read` for `caller` through `handle_conn`. Used by the
/// reply-watermark control so the watermark, not only the entries, crosses the
/// production socket boundary.
fn cad2207_thread_read_over_socket(fx: &Fx, state: &Path, caller: &str, params: Value) -> Value {
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let token = cad2207_socket_token(state);
    let request = cad2207_socket_frame("thread_read", params, caller, &token);
    let shared = fx.shared.clone();
    let server_thread = std::thread::spawn(move || {
        crate::daemon::serve::handle_conn(shared, server);
    });
    client
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    client.write_all(&request).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut raw = Vec::new();
    client.read_to_end(&mut raw).unwrap();
    server_thread.join().expect("socket dispatch thread");
    let text = String::from_utf8(raw).expect("socket response is UTF-8");
    let line = text
        .lines()
        .next()
        .unwrap_or_else(|| panic!("the socket returned no response line: {text:?}"));
    serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("the socket response is not one JSON frame ({error}): {line}")
    })
}

/// CAD-2207 (negative acceptance): once the owner's LOCAL unshare for the job
/// has SUCCEEDED, the already-selected notice must not continue arriving on a
/// blocked `member_assist_history` socket response. A coded
/// `share_withdrawal_pending` refusal is acceptable only while the share truly
/// remains live; in that case the response must still carry the authorized
/// notice. An unrelated shared job must be withdrawable in the same window.
#[test]
fn cad2207_blocked_history_response_cannot_outlive_a_completed_share_withdrawal() {
    let _flag = AssistantFlagOn::arm();
    let (fx, state) = cad2207_socket_fx();

    // One socket pair carries the REAL handle_conn. Pin BOTH buffer ends small
    // BEFORE the page is sized: the response must exceed what the kernel can
    // buffer, or the writer could finish before the withdrawal is issued.
    let (server, mut client) = UnixStream::pair().expect("socket pair");
    cad2207_socket_set_buffer(&server, libc::SO_SNDBUF, 4096);
    cad2207_socket_set_buffer(&client, libc::SO_RCVBUF, 4096);
    let send_buffer = cad2207_socket_buffer(&server, libc::SO_SNDBUF);
    let receive_buffer = cad2207_socket_buffer(&client, libc::SO_RCVBUF);
    assert!(
        send_buffer > 0 && receive_buffer > 0,
        "the fixture could not read effective socket buffers"
    );

    // Size the page from the EFFECTIVE buffers: three times their sum plus
    // 128 KiB of slack, spread over enough entries to stay inside PAGE_MAX.
    const FILLER_COUNT: usize = 200;
    let needed = (send_buffer as usize + receive_buffer as usize) * 3 + 128 * 1024;
    let filler_body = (needed / FILLER_COUNT).max(256);
    let scenario = cad2207_socket_history_fixture(&fx, FILLER_COUNT, filler_body);

    let params = json!({
        "member_envelope": fx.member_envelope(CAD2207_SOCKET_SUBSCRIBER),
        "limit": 500,
    });

    // Control 1: the SAME signed read in-process sees the receipt-linked notice
    // while the share is live. This proves "absent after" is a change, not a
    // notice that was never there.
    let control = fx.op("member_assist_history", params.clone());
    let control_entries = control["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("member_assist_history returned no entries: {control}"));
    assert!(
        control_entries
            .iter()
            .any(|entry| entry.get("message").and_then(Value::as_str)
                == Some(scenario.notice.id.as_str())),
        "the live signed history does not carry the receipt-linked notice: {control}"
    );
    assert!(
        control_entries.len() >= FILLER_COUNT,
        "the control page is smaller than the fixture's filler thread: {control}"
    );

    // Control 2 / setup proof: the exact frame the socket will frame is larger
    // than both effective buffers plus the prefix, so the writer cannot finish
    // before the withdrawal. (`handle_conn` adds only a small `fleet_protocol`.)
    let control_len = serde_json::to_string(&crate::proto::ok(control.clone()))
        .expect("control response serializes")
        .len();
    assert!(
        control_len
            > send_buffer as usize + receive_buffer as usize + CAD2207_SOCKET_PREFIX + 64 * 1024,
        "the synthetic page ({control_len} bytes) is not safely above the \
         socket buffers (snd={send_buffer}, rcv={receive_buffer}) plus the \
         prefix — the blocked-writer premise would not hold"
    );

    let token = cad2207_socket_token(&state);
    let request = cad2207_socket_frame("member_assist_history", params, "operator", &token);
    let done = Arc::new(AtomicBool::new(false));
    let shared = fx.shared.clone();
    let finished = done.clone();
    let server_thread = std::thread::spawn(move || {
        crate::daemon::serve::handle_conn(shared, server);
        finished.store(true, Ordering::SeqCst);
    });
    client
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    client.write_all(&request).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();

    // Wait for a real prefix of the history response. Nothing after the prefix
    // is read until both withdrawal verdicts are known.
    let prefix = cad2207_read_prefix(
        &mut client,
        CAD2207_SOCKET_PREFIX,
        "blocked history response",
    );
    assert!(
        !done.load(Ordering::SeqCst),
        "the history response completed before the withdrawal window opened — \
         the page/buffer sizing failed to keep the reply in flight"
    );
    let ok_needle: &[u8] = b"\"ok\":true";
    assert!(
        prefix
            .windows(ok_needle.len())
            .any(|window| window == ok_needle),
        "the response prefix is not a successful history frame: {}",
        String::from_utf8_lossy(&prefix)
    );
    for (label, secret) in [
        ("notice message id", scenario.notice.id.as_str()),
        ("notice body", scenario.notice.body.as_str()),
        ("assistant-origin source", CAD2207_SOCKET_NOTICE_SOURCE),
    ] {
        assert!(
            !String::from_utf8_lossy(&prefix).contains(secret),
            "the {label} arrived in the initially read response bytes — the \
             notice was not beyond the prefix"
        );
    }

    // The real withdrawals, both while the response is demonstrably in flight.
    let unrelated = cad2207_withdraw_bounded(&fx, &scenario.unrelated_job);
    let racing = cad2207_withdraw_bounded(&fx, &scenario.job);
    assert!(
        !done.load(Ordering::SeqCst),
        "the history response completed while the withdrawals were answered — \
         neither verdict overlapped the response, so this run proves nothing"
    );

    // Only now drain the bounded full response.
    let mut rest = Vec::new();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client
        .read_to_end(&mut rest)
        .unwrap_or_else(|error| panic!("draining the bounded full response failed: {error}"));
    server_thread.join().expect("socket dispatch thread");
    assert!(
        done.load(Ordering::SeqCst),
        "the socket dispatch never returned after the response was drained"
    );
    let mut all = prefix;
    all.extend_from_slice(&rest);
    let text = String::from_utf8(all).expect("socket response is UTF-8");
    let line = text
        .lines()
        .next()
        .unwrap_or_else(|| panic!("the socket returned no response line: {text:?}"));
    let response: Value = serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("the socket response is not one JSON frame ({error}): {line}")
    });

    // 1. An unrelated shared job must be withdrawable even while this response
    //    is in flight. A fix that serializes every share against one reply
    //    fails here.
    match &unrelated {
        Ok(answer) => {
            assert_eq!(
                answer["shared"],
                json!(false),
                "the unrelated withdrawal did not report a completed withdrawal: {answer}"
            );
            assert!(
                !fx.shared
                    .store
                    .assist_job_is_shared(&scenario.unrelated_job, ORG)
                    .unwrap(),
                "the unrelated withdrawal reported shared:false without clearing its row"
            );
        }
        Err(error) => panic!(
            "an UNRELATED shared job could not be withdrawn while a blocked history \
             response was in flight (timed_out={}, code={:?}): {}",
            error.timed_out, error.code, error.message
        ),
    }

    // 2. The racing withdrawal verdict decides which outcome is legitimate.
    let share_live = fx
        .shared
        .store
        .assist_job_is_shared(&scenario.job, ORG)
        .unwrap();
    let withheld = !text.contains(scenario.notice.id.as_str())
        && !text.contains(scenario.notice.body.as_str())
        && !text.contains(CAD2207_SOCKET_NOTICE_SOURCE);
    let refusal = response.pointer("/error/code").and_then(Value::as_str);
    let history_ok = response.get("ok").and_then(Value::as_bool) == Some(true);

    match &racing {
        Ok(answer) => {
            assert_eq!(
                answer["shared"],
                json!(false),
                "the racing withdrawal did not report a completed withdrawal: {answer}"
            );
            assert!(
                !share_live,
                "the racing withdrawal reported shared:false but the share row survived"
            );
            // A completed withdrawal must never be followed by the selected
            // notice. Either the response is an explicit pending refusal, or it
            // is the history page with the notice withheld.
            if history_ok {
                assert!(
                    withheld,
                    "the owner's share withdrawal COMPLETED while this response was \
                     still in flight, yet the blocked history response continued to \
                     deliver the private notice: {line}"
                );
                let entries = response
                    .pointer("/result/entries")
                    .and_then(Value::as_array)
                    .unwrap_or_else(|| panic!("the successful response has no entries: {line}"));
                assert!(
                    entries.len() >= FILLER_COUNT,
                    "the successful response lost the ordinary history page: {line}"
                );
                assert!(
                    entries.iter().any(|entry| entry
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.starts_with(CAD2207_SOCKET_FILLER))),
                    "the successful response carries no filler entry — it is not the \
                     real history page: {line}"
                );
                assert!(
                    !entries.iter().any(|entry| {
                        entry.get("message").and_then(Value::as_str)
                            == Some(scenario.notice.id.as_str())
                    }),
                    "the successful response still carries the withdrawn notice entry: {line}"
                );
            } else {
                assert_eq!(
                    refusal,
                    Some("share_withdrawal_pending"),
                    "a completed withdrawal was followed by an unrelated refusal rather \
                     than an explicit coded one: {line}"
                );
            }
        }
        Err(error) if error.code.as_deref() == Some("share_withdrawal_pending") => {
            assert!(
                share_live,
                "the daemon refused the withdrawal as pending but the share row was \
                 already gone — a false pending must not excuse the response: {line}"
            );
            if history_ok {
                assert!(
                    text.contains(scenario.notice.id.as_str()),
                    "the withdrawal was refused precisely because the share is still live, \
                     so the response must still carry the authorized notice: {line}"
                );
            } else {
                assert!(
                    refusal.is_some(),
                    "a failed history response must carry an explicit coded refusal, \
                     never a silent empty page: {line}"
                );
            }
        }
        Err(error) => panic!(
            "the owner's withdrawal neither completed nor answered the coded \
             `share_withdrawal_pending` refusal (timed_out={}): {}",
            error.timed_out, error.message
        ),
    }
}

/// CAD-2207 control (reply watermark across the socket): the self `thread_read`
/// `reply_seq` is derived from the WHOLE thread, so a revoked notice reply that
/// lies OUTSIDE the requested page must not be published as the watermark. The
/// page below starts after the notice reply; its `inserted` notice row is not a
/// candidate, yet an unfiltered watermark would still name it.
///
/// The two agent replies are seeded with the SAME `NewEntry` shape the actor
/// writes (`role=agent`, `kind=assistant_text`, `message_id` = the turn), and
/// both the live and withdrawn reads cross the REAL `handle_conn` socket. The
/// watermark filter itself already exists at this head, so this is a control
/// rather than a baseline-red acceptance; it pins the socket boundary.
#[test]
fn cad2207_revoked_notice_reply_watermark_does_not_cross_the_socket() {
    let _flag = AssistantFlagOn::arm();
    let (fx, state) = cad2207_socket_fx();

    // Both principals must have a stored, active binding before the owner's
    // signed job verbs can resolve (the same prelude the sibling fixtures use).
    let _owner = fx.member_runtime(CAD2207_SOCKET_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SOCKET_SUBSCRIBER);
    let alias = subscriber.alias.clone();
    let caller = format!("agent:{alias}");

    // An older, visible agent reply: the watermark that must survive.
    let own_message = "cad2207-history-socket-own-message";
    fx.shared
        .store
        .enqueue(
            &alias,
            "cad2207 history socket: the subscriber's own visible message",
            None,
            own_message,
            "test",
        )
        .unwrap();
    let visible_reply_seq = fx
        .shared
        .store
        .thread_append(
            &alias,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "cad2207 history socket: the own visible reply",
                payload: None,
                message_id: Some(own_message),
            },
        )
        .unwrap()
        .expect("the subscriber's bound thread exists");

    let (job, notice) = cad2207_socket_share_and_notice(
        &fx,
        &alias,
        CAD2207_SOCKET_REQUEST_KEY,
        "cad2207 history socket: the reply-watermark job",
    );

    // The newer agent reply is linked to the delivered notice — the exact shape
    // `Store::thread_reply_seq` treats as an agent reply.
    let notice_reply_seq = fx
        .shared
        .store
        .thread_append(
            &alias,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "cad2207 history socket: the notice reply",
                payload: None,
                message_id: Some(notice.id.as_str()),
            },
        )
        .unwrap()
        .expect("the subscriber's bound thread exists");
    assert!(
        notice_reply_seq > visible_reply_seq,
        "the notice reply ({notice_reply_seq}) is not newer than the visible own reply \
         ({visible_reply_seq}); the fixture did not order the watermark as intended"
    );

    // Control BEFORE the withdrawal, crossing the same socket: the public
    // watermark names the notice reply while the share is live.
    let live = cad2207_thread_read_over_socket(
        &fx,
        &state,
        &caller,
        json!({"alias": alias.as_str(), "limit": 200}),
    );
    assert_eq!(
        live.get("ok").and_then(Value::as_bool),
        Some(true),
        "the live self thread_read was refused over the socket: {live}"
    );
    assert_eq!(
        live.pointer("/result/reply_seq").and_then(Value::as_i64),
        Some(notice_reply_seq),
        "the live self thread_read did not name the notice reply in `result`: {live}"
    );

    // The REAL owner withdrawal, through the same signed member verb.
    let withdrawn = fx.op(
        "member_job_share",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_OWNER),
            "job": job,
            "enabled": false,
        }),
    );
    assert_eq!(
        withdrawn["shared"],
        json!(false),
        "member_job_share did not report the withdrawal: {withdrawn}"
    );
    assert!(
        !fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "member_job_share reported shared:false without clearing the share row"
    );

    // The page STARTS PAST the notice reply: the revoked row is outside the
    // requested page, yet the whole-thread watermark must not name it.
    let page = cad2207_thread_read_over_socket(
        &fx,
        &state,
        &caller,
        json!({"alias": alias.as_str(), "after": notice_reply_seq, "limit": 1}),
    );
    assert_eq!(
        page.get("ok").and_then(Value::as_bool),
        Some(true),
        "the post-withdrawal self thread_read was refused over the socket: {page}"
    );
    assert_eq!(
        page.pointer("/result/reply_seq").and_then(Value::as_i64),
        Some(visible_reply_seq),
        "the revoked notice reply's seq crossed the socket as `result.reply_seq` even \
         though its row was outside the requested page: {page}"
    );
    let page_text = page.to_string();
    assert!(
        !page_text.contains(notice.id.as_str()),
        "the post-withdrawal watermark response named the revoked notice id: {page}"
    );
    assert!(
        !page_text.contains(notice.body.as_str()),
        "the post-withdrawal watermark response named the revoked notice body: {page}"
    );

    // Positive control: the ordinary page still serves the member's own visible
    // reply and the notice (whose share is gone) stays withheld.
    let ordinary = cad2207_thread_read_over_socket(
        &fx,
        &state,
        &caller,
        json!({"alias": alias.as_str(), "after": 0, "limit": 200}),
    );
    assert!(
        ordinary
            .to_string()
            .contains("cad2207 history socket: the own visible reply"),
        "the post-withdrawal page dropped the member's own visible reply: {ordinary}"
    );
    assert!(
        !ordinary.to_string().contains(notice.id.as_str()),
        "the post-withdrawal page still served the revoked notice: {ordinary}"
    );
}

/// The state the in-flight reply-watermark race seeds.
struct Cad2207ReplyRace {
    alias: String,
    job: String,
    notice: crate::store::Message,
    notice_reply_seq: i64,
    visible_reply_seq: i64,
}

/// An older visible agent reply, the delivered notice, a newer agent reply
/// linked to that notice, then ordinary filler entries so a page that starts
/// AFTER the notice reply is large enough to keep a socket writer blocked.
/// The notice reply sits OUTSIDE that page, but `thread_read`'s whole-thread
/// watermark still sees it while the share is live.
fn cad2207_reply_race_fixture(
    fx: &Fx,
    filler_count: usize,
    filler_body: usize,
) -> Cad2207ReplyRace {
    let _owner = fx.member_runtime(CAD2207_SOCKET_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SOCKET_SUBSCRIBER);
    let alias = subscriber.alias.clone();

    let own_message = "cad2207-history-socket-reply-race-own";
    fx.shared
        .store
        .enqueue(
            &alias,
            "cad2207 reply race: the subscriber's own visible message",
            None,
            own_message,
            "test",
        )
        .unwrap();
    let visible_reply_seq = fx
        .shared
        .store
        .thread_append(
            &alias,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "cad2207 reply race: the own visible reply",
                payload: None,
                message_id: Some(own_message),
            },
        )
        .unwrap()
        .expect("the subscriber's bound thread exists");

    let (job, notice) = cad2207_socket_share_and_notice(
        fx,
        &alias,
        CAD2207_SOCKET_REPLY_RACE_KEY,
        "cad2207 history socket: the reply-watermark race job",
    );
    let notice_reply_seq = fx
        .shared
        .store
        .thread_append(
            &alias,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "cad2207 reply race: the notice reply",
                payload: None,
                message_id: Some(notice.id.as_str()),
            },
        )
        .unwrap()
        .expect("the subscriber's bound thread exists");
    assert!(
        notice_reply_seq > visible_reply_seq,
        "the notice reply ({notice_reply_seq}) is not newer than the own reply \
         ({visible_reply_seq}); the fixture did not order the watermark as intended"
    );

    // Ordinary entries AFTER the notice reply: the requested page starts past
    // the notice reply, so those entries alone size the blocked response.
    for index in 0..filler_count {
        let body = format!("{CAD2207_SOCKET_FILLER}{index}:{}", "x".repeat(filler_body));
        let id = format!("cad2207-history-socket-reply-filler-{index}");
        fx.shared
            .store
            .enqueue(&alias, &body, None, &id, "test")
            .unwrap_or_else(|error| panic!("could not seed reply-race filler {index}: {error}"));
    }

    Cad2207ReplyRace {
        alias,
        job,
        notice,
        notice_reply_seq,
        visible_reply_seq,
    }
}

/// CAD-2207 (negative acceptance, in-flight reply watermark): while a
/// `thread_read` response is blocked, the owner's unshare may complete. The
/// response's `result.reply_seq` was computed from the WHOLE thread while the
/// share was live and names the revoked notice reply — a row OUTSIDE the
/// returned page. After a completed withdrawal it must not cross the socket as
/// the live watermark (or the withdrawal must be an explicit
/// `share_withdrawal_pending` refusal while the share stays live).
#[test]
fn cad2207_blocked_reply_watermark_cannot_outlive_a_completed_withdrawal() {
    let _flag = AssistantFlagOn::arm();
    let (fx, state) = cad2207_socket_fx();

    let (server, mut client) = UnixStream::pair().expect("socket pair");
    cad2207_socket_set_buffer(&server, libc::SO_SNDBUF, 4096);
    cad2207_socket_set_buffer(&client, libc::SO_RCVBUF, 4096);
    let send_buffer = cad2207_socket_buffer(&server, libc::SO_SNDBUF);
    let receive_buffer = cad2207_socket_buffer(&client, libc::SO_RCVBUF);

    const FILLER_COUNT: usize = 200;
    let needed = (send_buffer as usize + receive_buffer as usize) * 3 + 128 * 1024;
    let filler_body = (needed / FILLER_COUNT).max(256);
    let race = cad2207_reply_race_fixture(&fx, FILLER_COUNT, filler_body);
    let alias = race.alias.clone();
    let caller = format!("agent:{alias}");

    // The page STARTS PAST the notice reply: its row is outside the page, yet
    // the in-process watermark (the whole thread) names it while shared.
    let params = json!({
        "alias": alias.as_str(),
        "after": race.notice_reply_seq,
        "limit": 500,
    });
    let control = fx
        .as_assistant(&alias, "thread_read", params.clone())
        .unwrap_or_else(|error| panic!("the live self thread_read was refused: {error}"));
    assert_eq!(
        control["reply_seq"].as_i64(),
        Some(race.notice_reply_seq),
        "the live control page did not carry the notice reply's watermark: {control}"
    );
    assert!(
        control["entries"]
            .as_array()
            .is_some_and(|entries| entries.len() >= FILLER_COUNT),
        "the control page is smaller than the fixture's filler thread: {control}"
    );

    let control_len = serde_json::to_string(&crate::proto::ok(control.clone()))
        .expect("control response serializes")
        .len();
    assert!(
        control_len
            > send_buffer as usize + receive_buffer as usize + CAD2207_SOCKET_PREFIX + 64 * 1024,
        "the reply-watermark page ({control_len} bytes) is not safely above the \
         socket buffers (snd={send_buffer}, rcv={receive_buffer}) plus the prefix"
    );

    let token = cad2207_socket_token(&state);
    let request = cad2207_socket_frame("thread_read", params, &caller, &token);
    let done = Arc::new(AtomicBool::new(false));
    let shared = fx.shared.clone();
    let finished = done.clone();
    let server_thread = std::thread::spawn(move || {
        crate::daemon::serve::handle_conn(shared, server);
        finished.store(true, Ordering::SeqCst);
    });
    client
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    client.write_all(&request).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();

    let prefix = cad2207_read_prefix(
        &mut client,
        CAD2207_SOCKET_PREFIX,
        "blocked reply watermark",
    );
    assert!(
        !done.load(Ordering::SeqCst),
        "the thread_read response completed before the withdrawal window opened"
    );
    let ok_needle: &[u8] = b"\"ok\":true";
    assert!(
        prefix
            .windows(ok_needle.len())
            .any(|window| window == ok_needle),
        "the response prefix is not a successful thread_read frame: {}",
        String::from_utf8_lossy(&prefix)
    );
    for secret in [race.notice.id.as_str(), race.notice.body.as_str()] {
        assert!(
            !String::from_utf8_lossy(&prefix).contains(secret),
            "the notice row/body arrived in the initially read reply-watermark bytes"
        );
    }

    let withdrawal = cad2207_withdraw_bounded(&fx, &race.job);
    assert!(
        !done.load(Ordering::SeqCst),
        "the thread_read response completed while the withdrawal was answered"
    );

    let mut rest = Vec::new();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
    client.read_to_end(&mut rest).unwrap_or_else(|error| {
        panic!("draining the bounded thread_read response failed: {error}")
    });
    server_thread.join().expect("socket dispatch thread");
    assert!(done.load(Ordering::SeqCst));
    let mut all = prefix;
    all.extend_from_slice(&rest);
    let text = String::from_utf8(all).expect("socket response is UTF-8");
    let line = text
        .lines()
        .next()
        .unwrap_or_else(|| panic!("the socket returned no response line: {text:?}"));
    let response: Value = serde_json::from_str(line).unwrap_or_else(|error| {
        panic!("the socket response is not one JSON frame ({error}): {line}")
    });

    let share_live = fx
        .shared
        .store
        .assist_job_is_shared(&race.job, ORG)
        .unwrap();
    let result = response.get("result");
    let reply_seq = result
        .and_then(|result| result.get("reply_seq"))
        .and_then(Value::as_i64);
    let refusal = response.pointer("/error/code").and_then(Value::as_str);

    match &withdrawal {
        Ok(answer) => {
            assert_eq!(
                answer["shared"],
                json!(false),
                "the withdrawal did not report a completed withdrawal: {answer}"
            );
            assert!(
                !share_live,
                "the withdrawal reported shared:false but the share row survived"
            );
            if response.get("ok").and_then(Value::as_bool) == Some(true) {
                assert_eq!(
                    reply_seq,
                    Some(race.visible_reply_seq),
                    "the revoked notice reply's seq remained in `result.reply_seq` after a \
                     completed withdrawal: {line}"
                );
                assert!(
                    !text.contains(race.notice.id.as_str())
                        && !text.contains(race.notice.body.as_str()),
                    "the post-withdrawal reply-watermark response named the revoked notice: {line}"
                );
                assert!(
                    result
                        .and_then(|result| result.get("entries"))
                        .and_then(Value::as_array)
                        .is_some_and(|entries| entries.len() >= FILLER_COUNT),
                    "the post-withdrawal reply-watermark page lost its ordinary entries: {line}"
                );
            } else {
                assert_eq!(
                    refusal,
                    Some("share_withdrawal_pending"),
                    "a completed withdrawal was followed by an unrelated refusal rather \
                     than an explicit coded one: {line}"
                );
            }
        }
        Err(error) if error.code.as_deref() == Some("share_withdrawal_pending") => {
            assert!(
                share_live,
                "the daemon refused the withdrawal as pending but the share row was gone: {line}"
            );
            if response.get("ok").and_then(Value::as_bool) == Some(true) {
                assert_eq!(
                    reply_seq,
                    Some(race.notice_reply_seq),
                    "the withdrawal was refused while the share is still live, so the \
                     response must still carry the authorized watermark: {line}"
                );
            } else {
                assert!(
                    refusal.is_some(),
                    "a failed reply-watermark response must carry an explicit code: {line}"
                );
            }
        }
        Err(error) => panic!(
            "the owner's withdrawal neither completed nor answered the coded \
             `share_withdrawal_pending` refusal (timed_out={}): {}",
            error.timed_out, error.message
        ),
    }
}

/// CAD-2207 (transport acceptance): a response that carries a live
/// receipt-linked notice takes the guarded NONBLOCKING bounded write path in
/// `handle_conn`, which must restore blocking mode before the connection reads
/// the next request. One real `handle_conn` connection carries TWO sequential
/// requests: a signed `member_assist_history` page that contains the live
/// notice (so `admit_history_response` returns a non-empty admission and the
/// guarded path runs), fully read WITHOUT half-closing, then a signed
/// `member_assist_state` on the SAME socket. If blocking mode were left off,
/// the daemon's next `reader.lines()` would see `WouldBlock`, close the
/// connection, and the second request would never receive its complete frame.
#[test]
fn cad2207_guarded_history_write_restores_blocking_for_the_next_request() {
    let _flag = AssistantFlagOn::arm();
    let (fx, state) = cad2207_socket_fx();

    let _owner = fx.member_runtime(CAD2207_SOCKET_OWNER);
    let subscriber = fx.member_runtime(CAD2207_SOCKET_SUBSCRIBER);
    let (job, notice) = cad2207_socket_share_and_notice(
        &fx,
        &subscriber.alias,
        CAD2207_SOCKET_SEQUENTIAL_KEY,
        "cad2207 history socket: the sequential-connection job",
    );
    assert!(
        fx.shared.store.assist_job_is_shared(&job, ORG).unwrap(),
        "the sequential fixture's live share is missing"
    );

    let (server, mut client) = UnixStream::pair().expect("socket pair");
    let shared = fx.shared.clone();
    let done = Arc::new(AtomicBool::new(false));
    let finished = done.clone();
    let server_thread = std::thread::spawn(move || {
        crate::daemon::serve::handle_conn(shared, server);
        finished.store(true, Ordering::SeqCst);
    });
    client
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(client.try_clone().expect("client clone"));
    let token = cad2207_socket_token(&state);

    // Request 1: the notice is a live receipt-linked entry in this page, so
    // `admit_history_response` holds its admission and the guarded nonblocking
    // write path runs. The client does NOT half-close the connection.
    let first = cad2207_socket_frame(
        "member_assist_history",
        json!({
            "member_envelope": fx.member_envelope(CAD2207_SOCKET_SUBSCRIBER),
            "limit": 50,
        }),
        "operator",
        &token,
    );
    client.write_all(&first).expect("first request write");
    client.flush().expect("first request flush");
    let mut first_line = String::new();
    let first_read = reader
        .read_line(&mut first_line)
        .expect("first response read");
    assert!(
        first_read > 0,
        "the guarded history request was never answered: {:?}",
        first_line
    );
    let first_response: Value =
        serde_json::from_str(first_line.trim_end()).unwrap_or_else(|error| {
            panic!("first response is not one JSON frame ({error}): {first_line}")
        });
    assert_eq!(
        first_response.get("ok").and_then(Value::as_bool),
        Some(true),
        "the first signed history request was refused: {first_line}"
    );
    assert!(
        first_response
            .pointer("/result/entries")
            .and_then(Value::as_array)
            .is_some_and(|entries| entries.iter().any(|entry| {
                entry.get("message").and_then(Value::as_str) == Some(notice.id.as_str())
            })),
        "the first response does not contain the live receipt-linked notice, so the \
         guarded admission write path was not exercised: {first_line}"
    );

    // Request 2 on the SAME connection, after the first frame was fully read.
    // A missing blocking-mode restoration closes the connection before this
    // request is read or answered.
    let second = cad2207_socket_frame(
        "member_assist_state",
        json!({"member_envelope": fx.member_envelope(CAD2207_SOCKET_SUBSCRIBER)}),
        "operator",
        &token,
    );
    client.write_all(&second).unwrap_or_else(|error| {
        panic!(
            "the second request could not be written on the SAME connection — the \
             guarded history write likely left the socket nonblocking: {error}"
        )
    });
    client.flush().unwrap_or_else(|error| {
        panic!(
            "the second request could not be flushed on the SAME connection — the \
             guarded history write likely left the socket nonblocking: {error}"
        )
    });
    let mut second_line = String::new();
    let second_read = reader
        .read_line(&mut second_line)
        .expect("second response read");
    assert!(
        second_read > 0 && !second_line.trim().is_empty(),
        "the second request on the SAME connection received no response — the guarded \
         history write likely left the socket nonblocking (handle_conn broke on the next \
         read); first frame: {first_line}"
    );
    let second_response: Value =
        serde_json::from_str(second_line.trim_end()).unwrap_or_else(|error| {
            panic!("second response is not one JSON frame ({error}): {second_line}")
        });
    assert_eq!(
        second_response.get("ok").and_then(Value::as_bool),
        Some(true),
        "the second signed request on the same connection was refused: {second_line}"
    );
    assert!(
        second_response
            .pointer("/result/conversation_id")
            .and_then(Value::as_str)
            .is_some_and(|conversation| !conversation.is_empty()),
        "the second response is not a complete member_assist_state frame: {second_line}"
    );

    client.shutdown(std::net::Shutdown::Write).unwrap();
    server_thread.join().expect("socket dispatch thread");
    assert!(
        done.load(Ordering::SeqCst),
        "the socket dispatch never returned after both sequential requests"
    );
}
