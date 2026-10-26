// Independent owner cad1085-security. Original CAD-1128 r2 contract:
// 3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8.
// One real supported SMTP lifecycle check; simulated operator setup is NOT native proof.
#[cfg(not(all(test, target_os = "linux", feature = "test-seam")))]
compile_error!("smtp_scope7 requires isolated admitted Linux test-seam execution");
use super::{Rig, SmtpCounts};
use crate::error::{Error, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Condvar, LazyLock, Mutex, TryLockError, Weak};
use std::thread::{self, Scope, ScopedJoinHandle};
use std::time::{Duration, Instant};

const A: &str = "YAwS5AyAesmZzdAuOBbBykufIyVNQQunwIqfTTWJlu4";
const B: &str = "Lc6BitPRSkgpooTCxJPhIuQaLtngFLkWw5MpFl5l4HY";
const WAIT: Duration = Duration::from_secs(10);
const HOLD: Duration = Duration::from_secs(6);

fn must<T, E>(r: std::result::Result<T, E>, stage: &'static str) -> T {
    r.unwrap_or_else(|_| panic!("{stage}"))
}
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::daemon) enum Point {
    BeforeAdmission,
    AfterProof,
    ArchiveBeforeRelease,
    ProviderAfterData,
}
#[derive(Default)]
struct GateState {
    entered: bool,
    released: bool,
    counts: Option<SmtpCounts>,
}
struct Gate {
    state: Mutex<GateState>,
    event: Condvar,
    pause: bool,
}
impl Gate {
    fn new(pause: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState::default()),
            event: Condvar::new(),
            pause,
        })
    }
    fn enter(&self, counts: Option<SmtpCounts>) -> Result<()> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| Error::internal("S1 gate poisoned"))?;
        s.entered = true;
        s.counts = counts;
        self.event.notify_all();
        let deadline = Instant::now() + HOLD;
        while self.pause && !s.released {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::internal("S1 owned gate release deadline"));
            }
            s = self
                .event
                .wait_timeout(s, remaining)
                .map_err(|_| Error::internal("S1 gate poisoned"))?
                .0;
        }
        Ok(())
    }
    fn entered(&self) -> Option<SmtpCounts> {
        let mut s = must(self.state.lock(), "S1 observation poisoned");
        let deadline = Instant::now() + WAIT;
        while !s.entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "S1 actual hook was not reached before deadline"
            );
            s = must(
                self.event.wait_timeout(s, remaining),
                "S1 observation poisoned",
            )
            .0;
        }
        s.counts
    }
    fn release(&self) {
        let mut s = must(self.state.lock(), "S1 release poisoned");
        s.released = true;
        self.event.notify_all();
    }
}
struct Probe {
    gates: Mutex<BTreeMap<Point, Arc<Gate>>>,
}
static PATHS: LazyLock<Mutex<BTreeMap<PathBuf, Weak<Probe>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static PORTS: LazyLock<Mutex<BTreeMap<u16, Weak<Probe>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
struct Owner {
    probe: Arc<Probe>,
    path: PathBuf,
    port: u16,
    armed: Mutex<Vec<Arc<Gate>>>,
}
impl Owner {
    fn install(rig: &Rig) -> Self {
        let probe = Arc::new(Probe {
            gates: Mutex::new(BTreeMap::new()),
        });
        assert!(must(PATHS.lock(), "S1 registry poisoned")
            .insert(rig.state_dir.clone(), Arc::downgrade(&probe))
            .is_none());
        assert!(must(PORTS.lock(), "S1 registry poisoned")
            .insert(rig.smtp_port, Arc::downgrade(&probe))
            .is_none());
        Self {
            probe,
            path: rig.state_dir.clone(),
            port: rig.smtp_port,
            armed: Mutex::new(Vec::new()),
        }
    }
    fn arm(&self, point: Point, pause: bool) -> Arc<Gate> {
        let gate = Gate::new(pause);
        assert!(
            must(self.probe.gates.lock(), "S1 arm poisoned")
                .insert(point, gate.clone())
                .is_none(),
            "S1 point already armed"
        );
        must(self.armed.lock(), "S1 arm ownership poisoned").push(gate.clone());
        gate
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Only this fixture's path/port and gates. Bounded client threads are joined below.
        for gate in self.armed.get_mut().unwrap_or_else(|e| e.into_inner()) {
            gate.release();
        }
        PATHS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.path);
        PORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.port);
    }
}
fn enter(probe: Option<Arc<Probe>>, point: Point, counts: Option<SmtpCounts>) -> Result<()> {
    if let Some(probe) = probe {
        let gate = probe
            .gates
            .lock()
            .map_err(|_| Error::internal("S1 hook poisoned"))?
            .remove(&point);
        if let Some(gate) = gate {
            gate.enter(counts)?;
        }
    }
    Ok(())
}
// Hooks are observation/pause ONLY; never choose caller, load identity, SQL outcome or SMTP reply.
pub(in crate::daemon) fn at(shared: &crate::daemon::Shared, point: Point) -> Result<()> {
    let probe = PATHS
        .lock()
        .map_err(|_| Error::internal("S1 registry poisoned"))?
        .get(&shared.state_dir)
        .and_then(Weak::upgrade);
    enter(probe, point, None)
}
pub(super) fn provider_at(port: u16, counts: SmtpCounts) -> Result<()> {
    let probe = PORTS
        .lock()
        .map_err(|_| Error::internal("S1 registry poisoned"))?
        .get(&port)
        .and_then(Weak::upgrade);
    enter(probe, Point::ProviderAfterData, Some(counts))
}
fn safe(text: &str) {
    assert!(
        !text.contains(A) && !text.contains(B),
        "S1 public result disclosed credential"
    );
}
fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    let result = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, WAIT)
    });
    match &result {
        Ok(v) => safe(&v.to_string()),
        Err(e) => safe(&e.to_string()),
    }
    result
}
fn send_params(rig: &Rig) -> Value {
    json!({"install_id":rig.install_id,"context_id":rig.context_id,"campaign_id":rig.campaign_id,"to_email":"recipient@example.test"})
}
fn archive_params(rig: &Rig) -> Value {
    json!({"install_id":rig.install_id,"context_id":rig.context_id,"expected_revision":1})
}
fn accepted(v: Value, id: &str) {
    assert_eq!(
        v["receipt"]["accepted"], true,
        "real peer did not accept supported send"
    );
    assert_eq!(v["receipt"]["pending_approval"], false);
    assert_eq!(v["receipt"]["connection_id"], id);
    assert_eq!(v["receipt"]["auth_revision"], 1);
}
fn context_refused(result: Result<Value>) {
    let error = result.expect_err("completed context invalidation allowed supported SMTP");
    assert_eq!(error.kind(), "rejected", "not a context admission refusal");
    assert!(
        error.code().is_none(),
        "transport/key/busy error is not context proof"
    );
    assert!(
        error.to_string().contains("context is archived"),
        "refusal did not observe the completed archive"
    );
}
fn delta_one(before: SmtpCounts, after: SmtpCounts) {
    assert_eq!(after.tcp_sessions, before.tcp_sessions + 1);
    assert_eq!(after.accepted_messages, before.accepted_messages + 1);
    assert_eq!(after.auth_a, before.auth_a + 1);
    assert_eq!(after.auth_b, before.auth_b);
    assert_eq!(after.auth_other, before.auth_other);
}
struct Pending<'scope> {
    rx: mpsc::Receiver<Result<Value>>,
    thread: Option<ScopedJoinHandle<'scope, ()>>,
}
impl Pending<'_> {
    fn pending(&self) {
        assert!(
            matches!(self.rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "mutator completed inside held supported send"
        );
    }
    fn finish(mut self) -> Result<Value> {
        let result = must(
            self.rx.recv_timeout(WAIT),
            "S1 concurrent real RPC completion deadline",
        );
        must(
            self.thread.take().unwrap().join(),
            "S1 concurrent real RPC panicked",
        );
        result
    }
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
fn task<'scope, 'env>(
    scope: &'scope Scope<'scope, 'env>,
    rig: &'scope Rig,
    method: &'static str,
    params: Value,
) -> Pending<'scope> {
    let (tx, rx) = mpsc::channel();
    let thread = scope.spawn(move || {
        let _ = tx.send(rpc(rig, method, params));
    });
    Pending {
        rx,
        thread: Some(thread),
    }
}
fn archived(v: &Value) {
    assert_eq!(v["context"]["state"], "archived");
    assert_eq!(v["context"]["revision"], 2);
}
fn release_free(rig: &Rig) -> bool {
    match rig.shared.app_release_lock.try_lock() {
        Ok(guard) => {
            drop(guard);
            true
        }
        Err(TryLockError::WouldBlock) => false,
        Err(TryLockError::Poisoned(_)) => panic!("actual R poisoned"),
    }
}
fn held_pipeline(rig: &Rig) {
    let pm_dir = must(rig.shared.pm_dir(), "S1 isolated PM path unavailable");
    assert_eq!(pm_dir, rig.state_dir.parent().unwrap().join("pm"));
    let pm = must(rig.shared.pm_at(&pm_dir), "S1 actual PM handle unavailable");
    assert!(
        must(pm.try_lock(), "S1 actual P observation failed").is_none(),
        "runtime P dropped before provider return"
    );
    assert!(
        !release_free(rig),
        "release R dropped before provider return"
    );
    let e = match rig.shared.custody_operation() {
        Err(e) => e,
        Ok(_) => panic!("custody C dropped before provider return"),
    };
    assert_eq!(e.code(), Some("busy"));
    let e =
        rpc(rig, "connection_custody_lock", json!({})).expect_err("held send allowed custody lock");
    assert_eq!(e.code(), Some("busy"));
    assert_eq!(
        rig.shared.platform_custody.encrypted().unwrap().status()["unlocked"],
        true
    );
}

#[test]
fn encrypted_smtp_scope_preserves_runtime_custody_release_order() {
    let root = must(
        tempfile::Builder::new()
            .prefix("s7-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(std::env::temp_dir()),
        "S1 private isolated root failed",
    );
    assert!(root.path().starts_with(std::env::temp_dir()));
    let mut rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "S1 real sole Rig setup failed",
    );
    for method in ["connection_custody_initialize", "connection_custody_unlock"] {
        must(
            rpc(&rig, method, json!({"accept_same_uid_risk":true})),
            "S1 actual custody setup failed",
        );
    }
    let created = must(
        rpc(
            &rig,
            "connection_create",
            json!({"provider":"smtp","account":"scope7","shape":"smtp","host":"localhost","port":rig.smtp_port,"tls_mode":"implicit","username":"fixture-user","secret":A,"sender":"sender@example.test","scopes":["email:send"],"accept_same_uid_risk":true}),
        ),
        "S1 actual enrollment failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("S1 enrollment id absent")
        .to_string();
    must(
        rpc(
            &rig,
            "crm_smtp_bind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,"connection_id":id,"request_id":"s7-bind"}),
        ),
        "S1 actual bind failed",
    );
    let core = must(rig.core_record(&id), "S1 exact core observation failed").to_json();
    let before = must(rig.smtp_barrier(), "S1 control barrier failed");
    accepted(
        must(
            rpc(&rig, "crm_smtp_test_send", send_params(&rig)),
            "S1 real verified TLS control failed",
        ),
        &id,
    );
    let after = must(rig.smtp_barrier(), "S1 control drain failed");
    delta_one(before, after);
    eprintln!("scope7 stage=real-control complete");
    let owner = Owner::install(&rig);
    let proof = owner.arm(Point::AfterProof, true);
    thread::scope(|scope| {
        let send = task(scope, &rig, "crm_smtp_test_send", send_params(&rig));
        proof.entered();
        // Real lock observation determines legal linearization, NOT an invented state model.
        // A wrong implementation releasing R admits an actual archive now; that branch
        // must refuse traffic. The baseline retains R and finishes the admitted send first.
        if release_free(&rig) {
            archived(&must(
                rpc(&rig, "app_context_archive", archive_params(&rig)),
                "S1 completed post-proof archive failed",
            ));
            proof.release();
            let result = send.finish();
            assert_eq!(
                must(rig.smtp_barrier(), "S1 mutation traffic drain failed"),
                after,
                "completed post-proof archive allowed actual SMTP traffic"
            );
            context_refused(result);
            panic!("S1 release guard missing even though a later guard refused");
        }
        let provider = owner.arm(Point::ProviderAfterData, true);
        let archive_entry = owner.arm(Point::ArchiveBeforeRelease, false);
        let link_entry = owner.arm(Point::BeforeAdmission, false);
        let archive = task(scope, &rig, "app_context_archive", archive_params(&rig));
        archive_entry.entered();
        let rebind = task(
            scope,
            &rig,
            "crm_smtp_rebind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,"connection_id":id,"expected_revision":1}),
        );
        link_entry.entered();
        proof.release();
        let wire = provider.entered().expect("S1 actual DATA counters absent");
        assert_eq!(wire.tcp_sessions, after.tcp_sessions + 1);
        assert_eq!(wire.auth_a, after.auth_a + 1);
        assert_eq!(wire.auth_b, after.auth_b);
        assert_eq!(wire.auth_other, after.auth_other);
        assert_eq!(
            wire.accepted_messages, after.accepted_messages,
            "peer was not paused before real DATA acceptance"
        );
        held_pipeline(&rig);
        archive.pending();
        rebind.pending();
        send.pending();
        assert_eq!(
            must(rig.core_record(&id), "S1 held core observation failed").to_json(),
            core
        );
        let context = must(
            rpc(
                &rig,
                "app_context_show",
                json!({"install_id":rig.install_id,"context_id":rig.context_id}),
            ),
            "S1 held context observation failed",
        );
        assert_eq!(context["context"]["state"], "active");
        provider.release();
        accepted(
            must(send.finish(), "S1 admitted real send did not finish"),
            &id,
        );
        archived(&must(
            archive.finish(),
            "S1 R-only archive did not complete",
        ));
        // P/C/R ordering permits either rebind before archive or archive before rebind.
        match rebind.finish() {
            Ok(_) => {
                let link = must(
                    rig.shared
                        .store
                        .crm_smtp_link(&rig.install_id, &rig.context_id),
                    "S1 actual link observation failed",
                )
                .expect("S1 link disappeared");
                assert_eq!(link.link_revision, 2);
                assert_eq!(link.connection_id, id);
            }
            Err(e) => context_refused(Err(e)),
        }
    });
    let drained = must(rig.smtp_barrier(), "S1 held-provider drain failed");
    delta_one(after, drained);
    context_refused(rpc(&rig, "crm_smtp_test_send", send_params(&rig)));
    assert_eq!(
        must(rig.smtp_barrier(), "S1 archived refusal drain failed"),
        drained
    );
    eprintln!("scope7 stage=held-provider-and-concurrent-link-archive complete");
    // Independent pre-admission ordering: COMPLETE actual invalidation before P/C/R/D proof.
    let context = must(
        rpc(
            &rig,
            "app_context_create",
            json!({"install_id":rig.install_id,"label":"Scope","input_defaults":{},"request_id":"s7-pre"}),
        ),
        "S1 second real context setup failed",
    );
    rig.context_id = context["context"]["id"]
        .as_str()
        .expect("S1 new context id absent")
        .to_string();
    must(
        rpc(
            &rig,
            "app_content_save",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,"campaign_id":rig.campaign_id,"subject":"Hi","preheader":"Pre","blocks":[{"type":"paragraph","text":"Body."}]}),
        ),
        "S1 second actual content setup failed",
    );
    must(
        rpc(
            &rig,
            "crm_smtp_bind",
            json!({"install_id":rig.install_id,"context_id":rig.context_id,"connection_id":id,"request_id":"s7-bind-pre"}),
        ),
        "S1 second actual binding setup failed",
    );
    let admission = owner.arm(Point::BeforeAdmission, true);
    thread::scope(|scope| {
        let send = task(scope, &rig, "crm_smtp_test_send", send_params(&rig));
        admission.entered();
        archived(&must(
            rpc(&rig, "app_context_archive", archive_params(&rig)),
            "S1 actual pre-admission archive did not complete",
        ));
        admission.release();
        context_refused(send.finish());
    });
    assert_eq!(
        must(rig.smtp_barrier(), "S1 pre-admission refusal drain failed"),
        drained,
        "completed pre-admission archive started SMTP traffic"
    );
    assert_eq!(
        must(rig.core_record(&id), "S1 final core observation failed").to_json(),
        core
    );
    assert!(release_free(&rig), "S1 R leaked after completion");
    let c = must(
        rig.shared.custody_operation(),
        "S1 C leaked after completion",
    );
    drop(c);
    let pm = must(
        rig.shared.pm_at(&root.path().join("pm")),
        "S1 final PM handle failed",
    );
    assert!(
        must(pm.try_lock(), "S1 final P observation failed").is_some(),
        "S1 P leaked after completion"
    );
    eprintln!("scope7 stage=pre-admission-invalidation-zero-traffic complete");
    drop(owner);
    must(rig.finish(), "S1 actual owned fixture teardown failed");
}
