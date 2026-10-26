// Independent test author: cad1128-runtime-swe — original-r2 unit 4
// (transmitted-send timeout truthfulness / no-resend). Derived from contract
// r2 `connection_custody_transmitted_send_timeout_does_not_resend` and the
// coverage-disposition finite unit 4. Mounted unchanged by the sole writer.
// This driver owns every assertion; the fixture owns only isolated
// setup/teardown plus the ONE-LINE hold seam described in integration.md.
//
// Mount as `crate::daemon::custody_acceptance_fixture::smtp_timeout4`
// under `#[cfg(all(test, unix, feature = "test-seam"))]`.
//
// Contract sha256:
// 3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8
//
// REUSE: this driver rides the EXISTING real TLS Rig and its real AUTH/
// MAIL/RCPT/DATA fixture session. The only new test-only mechanism is a
// bounded post-DATA reply-hold seam that the sole writer inserts at the
// same place `smtp_scope7::provider_at` already sits (after `.` consumed,
// before `accepted_messages`/`reply("250 queued")`). No fake SmtpOutcome,
// no clock/deadline/timeout change — the peer's hold is the only delay and
// it exceeds the client's real configured 15 s IO_TIMEOUT.

#[cfg(not(all(test, unix, feature = "test-seam")))]
compile_error!("smtp_timeout4 requires isolated admitted test-seam execution");

use super::Rig;
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::{mpsc, Arc, Condvar, LazyLock, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

// Synthetic secrets; only fixed diagnostics ever leave the driver.
const SECRET: &str = "cad1128-runtime-A-f3a8c1d9";
const OTHER: &str = "cad1128-runtime-B-91b7e204";

const WAIT: Duration = Duration::from_secs(25);

// ---------------------------------------------------------------------------
// The owned post-DATA reply-hold seam (test-only). Registry is keyed on the
// fixture peer's local port — exactly how `smtp_scope7` keys its Provider
// gate — and armed by the driver per test. `hold_reply` blocks the genuine
// fixture's `250 queued` until `release_reply` or its own deadline; it never
// picks a reply, a credential, a caller, or an outcome.
// ---------------------------------------------------------------------------

struct Hold {
    state: Mutex<HoldState>,
    event: Condvar,
}
#[derive(Default)]
struct HoldState {
    /// The peer consumed `.` and is now parked — the request transmitted.
    entered: bool,
    /// The driver released the hold — the `250` proceeds as a late accept.
    released: bool,
    /// The peer's own 40 s cap elapsed before any release — the hold is
    /// spent. An unexpired hold is REQUIRED for the falsifier: a parked
    /// peer that timed out silently would let a committed revoke land
    // after the accept, masking the bug.
    expired: bool,
    /// The peer finished `hold_reply` — released or expired. Lets the
    /// driver confirm the peer actually observed the hold window.
    done: bool,
}
impl Hold {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HoldState::default()),
            event: Condvar::new(),
        })
    }
}
static HOLDS: LazyLock<Mutex<BTreeMap<u16, Weak<Hold>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Armed by the driver for one rig's smtp port. `Drop` releases any
/// unreleased hold and unregisters the port so a later test can re-arm.
struct HoldOwner {
    port: u16,
    hold: Arc<Hold>,
}
impl HoldOwner {
    fn arm(rig: &Rig) -> Self {
        let hold = Hold::new();
        let mut map = HOLDS.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            map.insert(rig.smtp_port, Arc::downgrade(&hold)).is_none(),
            "a reply-hold is already armed on this rig's port"
        );
        Self {
            port: rig.smtp_port,
            hold,
        }
    }
    /// Release the peer's held `250` — the late remote acceptance.
    fn release(&self) {
        let mut s = self.hold.state.lock().unwrap_or_else(|e| e.into_inner());
        s.released = true;
        self.hold.event.notify_all();
    }
    /// The peer consumed DATA and is STILL parked inside its (unexpired)
    /// hold — the only phase in which a committed revoke is guaranteed to
    // land before any remote acceptance.
    fn held_and_fresh(&self) -> bool {
        let s = self.hold.state.lock().unwrap_or_else(|e| e.into_inner());
        s.entered && !s.released && !s.expired && !s.done
    }
    /// ATOMIC held-phase release: under one lock, release ONLY if the
    /// peer is still parked inside an unexpired hold. Returns true iff the
    /// release happened while `entered && !expired && !done` — the
    /// decisive falsifier cannot observe a stale phase then release a
    /// spent hold.
    fn release_if_held(&self) -> bool {
        let mut s = self.hold.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.entered && !s.released && !s.expired && !s.done {
            s.released = true;
            self.hold.event.notify_all();
            true
        } else {
            s.released = true;
            self.hold.event.notify_all();
            false
        }
    }
    /// Wait until the peer has consumed DATA and parked on the hold — the
    /// only in-flight signal that does NOT join the worker (the barrier
    /// joins, so it cannot observe a held session).
    fn entered(&self) {
        let deadline = Instant::now() + WAIT;
        let mut s = self.hold.state.lock().unwrap_or_else(|e| e.into_inner());
        while !s.entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "peer never consumed DATA / never parked on the hold"
            );
            s = self
                .hold
                .event
                .wait_timeout(s, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}
impl Drop for HoldOwner {
    fn drop(&mut self) {
        self.release();
        HOLDS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.port);
    }
}

/// Owned join guard for the send helper — joins on EVERY unwind path so a
/// helper panic/return is never detached (its `JoinHandle` would otherwise
/// leak on an early `panic!`/`assert!`). `take` yields the raw handle so
/// the explicit join can propagate the panic payload.
struct SendJoin {
    inner: Option<thread::JoinHandle<()>>,
    /// A clone of the hold Arc so a cleanup join can release the peer's
    /// parked `250` FIRST — a join on a failed helper must never wait
    /// behind a still-held hold.
    hold: Arc<Hold>,
}
impl SendJoin {
    fn take(&mut self) -> thread::JoinHandle<()> {
        self.inner.take().expect("send handle already consumed")
    }
}
impl Drop for SendJoin {
    fn drop(&mut self) {
        // Release the peer's hold BEFORE joining: on an early panic the
        // helper may itself be waiting on the held session; joining while
        // it is still held would wedge the teardown.
        {
            let mut s = self.hold.state.lock().unwrap_or_else(|e| e.into_inner());
            s.released = true;
            self.hold.event.notify_all();
        }
        if let Some(join) = self.inner.take() {
            let _ = join.join();
        }
    }
}

/// Called by the genuine fixture DATA branch (see integration.md) at the
/// same point `smtp_scope7::provider_at` runs: after `.` consumed, before
/// `accepted_messages`/`reply`. Blocks until released or `hold_max`
/// elapses; the deadline is generous enough to outlive the client's real
/// 15 s IO_TIMEOUT with room for the driver's observation, then it lets
/// the peer's normal `250 queued` proceed so the acceptance is genuinely
/// late, never faked. Returns Ok so a missing/disarmed hold is a no-op —
/// the default fixture behavior is byte-identical.
pub(in crate::daemon) fn hold_reply(port: u16, hold_max: Duration) -> Result<()> {
    let hold = HOLDS
        .lock()
        .map_err(|_| Error::internal("rt4 hold registry poisoned"))?
        .get(&port)
        .and_then(Weak::upgrade);
    if let Some(hold) = hold {
        let deadline = Instant::now() + hold_max;
        let mut s = hold
            .state
            .lock()
            .map_err(|_| Error::internal("rt4 hold poisoned"))?;
        // Signal DATA-consumed-and-parked, then wait for release.
        s.entered = true;
        hold.event.notify_all();
        while !s.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // The 40 s cap elapsed with no release — mark the hold
                // spent so the driver can distinguish "still parked" from
                // "cap ran out", then let the `250` proceed anyway so a
                // stuck test never wedges the wire.
                s.expired = true;
                s.done = true;
                return Ok(());
            }
            s = hold
                .event
                .wait_timeout(s, remaining)
                .map_err(|_| Error::internal("rt4 hold poisoned"))?
                .0;
        }
        // Released — the peer completed the hold unexpired.
        s.done = true;
    }
    Ok(())
}

fn must<T, E>(r: std::result::Result<T, E>, stage: &'static str) -> T {
    match r {
        Ok(v) => v,
        Err(_) => panic!("{stage}"),
    }
}
fn safe_v(v: &Value) {
    let s = v.to_string();
    assert!(
        !s.contains(SECRET) && !s.contains(OTHER),
        "a public RPC result disclosed a synthetic credential"
    );
}
fn safe_e(e: &Error) {
    let s = format!("{e:?} {e}");
    assert!(
        !s.contains(SECRET) && !s.contains(OTHER),
        "a public RPC error disclosed a synthetic credential"
    );
}
fn call(state_dir: &std::path::Path, method: &str, params: Value, secs: u64) -> Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(state_dir, method, params, Duration::from_secs(secs))
    })
}
fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    let answer = call(&rig.state_dir, method, params, 60);
    match &answer {
        Ok(v) => safe_v(v),
        Err(e) => safe_e(e),
    }
    answer
}
fn refused(answer: Result<Value>, stage: &'static str) -> Error {
    match answer {
        Err(e) => e,
        Ok(_) => panic!("{stage}"),
    }
}
fn send_params(rig: &Rig) -> Value {
    json!({
        "install_id": rig.install_id,
        "context_id": rig.context_id,
        "campaign_id": rig.campaign_id,
        "to_email": "recipient@example.test",
    })
}

/// Original r2 `connection_custody_transmitted_send_timeout_does_not_resend`:
/// the REAL fixture peer consumes the DATA body, then its terminal `250` is
/// held through the actual configured 15 s client timeout so the request was
/// provably transmitted while the reply timed out — a truthful uncertain
/// local result, never a provably-not-sent claim. A busy mutation while the
/// send is held has no deferred write; a real revoke then commits while the
/// peer's late `250` is the permitted remote acceptance, and no resend or
/// retired-incarnation traffic runs.
#[test]
fn connection_custody_transmitted_send_timeout_does_not_resend() {
    let root = must(
        tempfile::Builder::new()
            .prefix("rt4-")
            .tempdir_in(std::env::temp_dir()),
        "private isolated root failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "private root mode failed",
    );
    let rig = must(
        Rig::start(root.path(), SECRET.as_bytes(), OTHER.as_bytes()),
        "real isolated rig setup failed",
    );
    for verb in ["connection_custody_initialize", "connection_custody_unlock"] {
        must(
            rpc(&rig, verb, json!({"accept_same_uid_risk": true})),
            "real custody setup failed",
        );
    }
    // Supported encrypted enrollment at the REAL fixture peer — the send
    // transmits genuine DATA over its real TLS session.
    let created = must(
        rpc(
            &rig,
            "connection_create",
            json!({
                "provider": "smtp", "account": "rt4", "shape": "smtp",
                "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
                "username": "fixture-user", "secret": SECRET,
                "sender": "sender@example.test", "sender_name": "Fixture Sender",
                "scopes": ["email:send"], "accept_same_uid_risk": true,
            }),
        ),
        "real supported enrollment failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("enrollment id absent")
        .to_string();
    must(
        rpc(
            &rig,
            "crm_smtp_bind",
            json!({"install_id": rig.install_id, "context_id": rig.context_id,
                   "connection_id": id, "request_id": "rt4-bind"}),
        ),
        "real bind failed",
    );

    // Positive control first: with NO hold armed, a real send is accepted
    // — proves the happy path and that the seam is inert when disarmed.
    let before = must(rig.smtp_barrier(), "pre-control SMTP barrier failed");
    let control = must(
        rpc(&rig, "crm_smtp_test_send", send_params(&rig)),
        "real happy control send failed",
    );
    assert_eq!(control["receipt"]["accepted"], json!(true));
    let after_control = must(rig.smtp_barrier(), "control drain failed");
    assert_eq!(
        after_control.accepted_messages,
        before.accepted_messages + 1
    );
    assert_eq!(after_control.auth_a, before.auth_a + 1);

    // ------------------------------------------------------------------
    // Arm the post-DATA reply hold on this rig's peer port, then fire the
    // real send on a helper thread. The peer consumes DATA then sits in
    // `hold_reply` until released — the client's read rides its real 15 s
    // deadline while the request is already transmitted.
    // ------------------------------------------------------------------
    let owner = HoldOwner::arm(&rig);
    let send_state = rig.state_dir.clone();
    let send_args = send_params(&rig);
    let (stx, srx) = mpsc::channel();
    let send_handle = SendJoin {
        inner: Some(thread::spawn(move || {
            let _ = stx.send(call(&send_state, "crm_smtp_test_send", send_args, 60));
        })),
        // A clone of the same hold — the cleanup join releases the peer
        // FIRST so a failed/held helper never waits behind the park.
        hold: owner.hold.clone(),
    };

    // Wait for the peer to consume DATA and park on the hold — the
    // daemon is then provably inside its held P→C→R→D section with a
    // transmitted request. The barrier cannot observe this (it joins the
    // held worker); the hold's own `entered` signal is the observation.
    owner.entered();
    // A mutation now must refuse busy — never a deferred write.
    let busy_answer = rpc(
        &rig,
        "connection_revoke",
        json!({"connection_id": id, "expected_connection_id": id,
               "expected_revision": 1}),
    );
    let busy_err = match busy_answer {
        Err(e) => e,
        Ok(_) => panic!("mutation committed inside the held supported send"),
    };
    assert_eq!(busy_err.kind(), "busy", "held-send mutation was not busy");
    // No deferred write: the record is still live at revision 1.
    let live = must(rig.core_record(&id), "post-busy core read failed");
    assert_eq!(
        live.credential_revision, 1,
        "busy mutation mutated authority"
    );

    // Let the client ride its real 15 s IO_TIMEOUT to a return. The peer is
    // STILL holding the `250` — the remote has not yet accepted.
    let outcome = match srx.recv_timeout(Duration::from_secs(45)) {
        Ok(o) => o,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("send completion deadline: the send helper did not return within 45 s")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("send completion: the send helper thread died without returning")
        }
    };
    // Owned join on the send helper — propagate its panic payload, never
    // swallow a helper failure. `take` consumes the handle; `send_handle`'s
    // Drop is a no-op afterwards.
    let mut send_join = send_handle;
    if let Err(panicked) = send_join.take().join() {
        std::panic::resume_unwind(panicked);
    }
    // Screen the asynchronous send outcome too — a raw `call` result must
    // not smuggle a secret just because `rpc()` screened only the
    // synchronous answer.
    match &outcome {
        Ok(v) => safe_v(v),
        Err(e) => safe_e(e),
    }

    // ---------------------------------------------------------------------
    // The send has returned. Before releasing the peer's late `250`, prove
    // the custody section is genuinely closed: a fresh explicit
    // `connection_revoke` must COMMIT (no longer busy) while the remote has
    // still not accepted. This ordering is the falsifier — it makes a
    // flattened `rejected`/not-sent result demonstrably wrong: the request
    // was transmitted, the send is fully finished (revoke committed), and
    // only afterward does the remote accept. If the handler had truthfully
    // reported `unknown` rather than the flattened `rejected`, the result
    // matches; if it falsely claimed not-sent, the subsequent real `250`
    // acceptance proves the transmission happened.
    // ---------------------------------------------------------------------
    // The revoke commits only once the send's custody section is closed.
    // Capture the RPC's own result so `revoked:true` is the positive
    // observable, not a bare Ok.
    let revoke_result = must(
        rpc(
            &rig,
            "connection_revoke",
            json!({"connection_id": id, "expected_connection_id": id,
                   "expected_revision": 1}),
        ),
        "explicit revoke after send return failed (custody section still held)",
    );
    assert_eq!(
        revoke_result["revoked"],
        json!(true),
        "connection_revoke did not report a committed revocation"
    );
    // The revoke COMMITTED while the peer was still parked inside its
    // (unexpired) hold — proving the remote had NOT yet accepted at the
    // moment the send's custody section closed. `held_and_fresh` is the
    // atomic held-phase observation; the matching `release_if_held` below
    // re-checks ATOM with the release so a spent hold can never pass.
    assert!(
        owner.held_and_fresh(),
        "the peer's hold expired or finished before the committed revoke — \
         the late-accept ordering is not provable"
    );
    // The encrypted revoke deletes the credential record. Prove the
    // authority is absent through positive observers — `connection_show`
    // must refuse specifically because the record is gone (unavailable or
    // stale), not on a transport/read failure, and `connection_list` must
    // succeed and no longer carry the id (a well-formed empty authority
    // projection, not a skipped/errored read).
    let show_err = match rpc(&rig, "connection_show", json!({"connection_id": id})) {
        Err(e) => e,
        Ok(_) => panic!("connection still resolves after a committed revoke"),
    };
    let show_text = show_err.to_string();
    assert!(
        show_text.contains("unavailable") || show_text.contains("stale"),
        "connection_show refused for an unexpected reason after revoke: {show_text}"
    );
    let list = must(
        rpc(&rig, "connection_list", json!({})),
        "connection_list failed after a committed revoke",
    );
    assert!(
        !list.to_string().contains(&id),
        "a committed revocation left the connection listed"
    );

    // ---------------------------------------------------------------------
    // LATE ACCEPTANCE MUST BE ESTABLISHED BEFORE THE DECISIVE ASSERT.
    // ATOMIC held-phase release: `release_if_held` releases ONLY while the
    // peer is still parked inside an unexpired hold, and returns whether
    // that held phase was live AT the moment of release — the observer
    // RPCs between the check and this call cannot let a silently-spent
    // hold slip through, because the release re-verifies the phase under
    // the same lock. Its genuine `250 queued` then lands late.
    // ---------------------------------------------------------------------
    assert!(
        owner.release_if_held(),
        "the peer's hold was not still-live at release — the remote accept \
         ordering is not provable"
    );
    // Exactly one session/DATA was ever consumed; no resend or foreign
    // auth happened — the full `SmtpCounts` observation, not a partial
    // subset, so an extra connect or a wrong-B auth cannot escape.
    let drained = must(rig.smtp_barrier(), "post-timeout SMTP drain failed");
    assert_eq!(
        drained.tcp_sessions,
        after_control.tcp_sessions + 1,
        "a second TCP session ran (resend or foreign connect)"
    );
    assert_eq!(drained.auth_a, after_control.auth_a + 1);
    assert_eq!(
        drained.auth_b, after_control.auth_b,
        "a foreign account-B auth ran"
    );
    assert_eq!(drained.auth_other, after_control.auth_other);
    // accepted_messages ticks when the peer's late 250 proceeds — this is
    // the decisive remote-accept observable.
    assert_eq!(
        drained.accepted_messages,
        after_control.accepted_messages + 1,
        "the peer's single late acceptance was not observed"
    );
    drop(owner);

    // ---------------------------------------------------------------------
    // Decisive oracle — evaluated only after the peer's late acceptance is
    // an established counter fact above. The remote DID accept, so a
    // `rejected`/`unknown` classification that reads as not-sent is
    // provably wrong. The contract's truthful uncertainty is
    // `Error::OutcomeUnknown` (constructor `Error::unknown`, wire
    // `kind="unknown"`, proto/CLI exit 1); the un-repaired handler erases
    // `Uncertain` into `Error::rejected` (exit 3) BEFORE the receipt is
    // built (crm_smtp_rpc.rs:722-729 / :768/:813). `rejected` must
    // ACTUALLY FAIL — it is the bug surface, not a passable result.
    // ---------------------------------------------------------------------
    match &outcome {
        Err(e) => {
            let kind = e.kind();
            eprintln!("rt4 observed send outcome kind={kind}");
            assert_eq!(
                kind, "unknown",
                "transmitted-send timeout was flattened to `{kind}` (the \
                 Uncertain→Rejected bug) even though the peer accepted; the \
                 truthful surface is `unknown`"
            );
            let text = e.to_string();
            assert!(
                !text.contains("not submitted")
                    && !text.contains("never sent")
                    && !text.contains("not_sent"),
                "timeout falsely reported the transmitted request as not sent"
            );
        }
        Ok(v) => {
            // The truthful-uncertain surface is REQUIRED on every branch —
            // an `Ok` result must carry an explicit uncertainty marker
            // (e.g. `uncertain:true` / an outcome field naming the
            // uncertain state); a bare non-accepted receipt or `{}` loses
            // it and is itself a wrong surface. Today's code cannot
            // produce `Ok`+uncertain (Uncertain is flattened to Err before
            // the receipt), so reaching here already means a wrong or
            // prematurely-accepted result.
            let receipt = &v["receipt"];
            assert_eq!(
                receipt["uncertain"],
                json!(true),
                "an unconfirmed transmission returned Ok without an explicit \
                 uncertain marker — the truthful surface is `unknown`"
            );
            assert_ne!(
                receipt["accepted"],
                json!(true),
                "an unconfirmed transmission produced a positive acceptance receipt"
            );
            assert_ne!(
                receipt["pending_approval"],
                json!(true),
                "a direct send reported hosted pending-approval"
            );
        }
    }

    // A fresh send must refuse — no new retired-incarnation traffic.
    let pre_resend = must(rig.smtp_barrier(), "pre-resend barrier failed");
    refused(
        rpc(&rig, "crm_smtp_test_send", send_params(&rig)),
        "a send ran against the revoked connection",
    );
    assert_eq!(
        must(rig.smtp_barrier(), "post-resend barrier failed"),
        pre_resend,
        "a retired-incarnation send started SMTP traffic"
    );

    must(rig.finish(), "owned fixture teardown failed");
}
