//! CAD-2215: explicit, authorized publication of a private delivery's
//! selected results to the one shared ticket it is bound to.
//!
//! Publication is an act, never an inference and never a side effect:
//!
//! - The caller selects the exact result message ids. There is no "all"
//!   shorthand, so nothing is shared that was not named.
//! - The daemon supplies a typed [`VerifiedPublisher`] — a principal
//!   that cannot be built from request fields or a raw `(org, sub)`
//!   claim, and is never taken from a board/loopback socket that merely
//!   relays a reader. The only production principal is the local proven
//!   operator, and publication re-checks the ticket and the project
//!   policy that admits the operator for issue writes. There is no
//!   job-ownership shortcut and no caller-supplied `explicit` flag
//!   (there is no such field).
//! - The only representable target is a shared **local board comment**.
//!   A PR effect has no contract — no authorized, head/selection-bound
//!   remote action exists — so it is not representable and stays
//!   refused. This module never touches `delivery.json`, so the ordinary
//!   review loop's state, Needs-you rows and PR observations are
//!   untouched.
//! - The receipt is recorded *before* the shared write and settled
//!   after it. A crash between the two leaves a `pending` receipt: an
//!   unknown outcome the operator reconciles, never a silent retry or a
//!   second comment.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::private::{self, PrivateDelivery, PrivateState};
use crate::error::{Error, Result};
use crate::issue::Pm;

/// Test-only deterministic interleaving seam for the publication claim.
/// Production is compiled without this module.
#[cfg(all(test, feature = "test-seam"))]
pub mod race_seam {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, LazyLock, Mutex};

    type Hook = Arc<dyn Fn() + Send + Sync>;

    static HOOKS: LazyLock<Mutex<BTreeMap<PathBuf, Hook>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));

    /// State-dir-scoped RAII registration. Drop removes the hook it
    /// installed only if it is still the registered one — a later
    /// registration for the same state dir is never removed by an older
    /// guard.
    pub struct PublicationRaceSeam {
        state_dir: PathBuf,
        hook: Hook,
    }

    impl Drop for PublicationRaceSeam {
        fn drop(&mut self) {
            let Ok(mut hooks) = HOOKS.lock() else {
                return;
            };
            let ours = hooks
                .get(&self.state_dir)
                .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook));
            if ours {
                hooks.remove(&self.state_dir);
            }
        }
    }

    /// Register `hook` for publication races on `state_dir`. The hook is
    /// invoked once per `publish`, immediately before `begin_publication`
    /// takes the ledger lock and after the real stale-read checks. The
    /// registry lock is released before the hook runs.
    pub fn arm_publication_race(state_dir: &Path, hook: Hook) -> PublicationRaceSeam {
        let state_dir = state_dir.to_path_buf();
        if let Ok(mut hooks) = HOOKS.lock() {
            hooks.insert(state_dir.clone(), Arc::clone(&hook));
        }
        PublicationRaceSeam { state_dir, hook }
    }

    pub(super) fn fire(state_dir: &Path) {
        let hook = HOOKS
            .lock()
            .ok()
            .and_then(|hooks| hooks.get(state_dir).cloned());
        if let Some(hook) = hook {
            hook();
        }
    }

    type PendingHook = Arc<dyn Fn(&super::PublicationReceipt, &str) + Send + Sync>;
    static POST_PENDING: LazyLock<Mutex<BTreeMap<PathBuf, PendingHook>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    static PRE_TRACKER: LazyLock<Mutex<BTreeMap<PathBuf, Hook>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    static MEMBER_DELIVERY: LazyLock<Mutex<BTreeMap<PathBuf, Hook>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));

    /// RAII registration for the member `private_deliveries` post-projection
    /// point, exposed from this public module so sibling `ui` tests can
    /// reach it without touching the private `daemon::members_rpc` module.
    pub struct MemberDeliverySeam {
        state_dir: PathBuf,
        hook: Hook,
    }

    impl Drop for MemberDeliverySeam {
        fn drop(&mut self) {
            let Ok(mut hooks) = MEMBER_DELIVERY.lock() else {
                return;
            };
            let ours = hooks
                .get(&self.state_dir)
                .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook));
            if ours {
                hooks.remove(&self.state_dir);
            }
        }
    }

    pub fn arm_member_delivery_post_projection(state_dir: &Path, hook: Hook) -> MemberDeliverySeam {
        let state_dir = state_dir.to_path_buf();
        if let Ok(mut hooks) = MEMBER_DELIVERY.lock() {
            hooks.insert(state_dir.clone(), Arc::clone(&hook));
        }
        MemberDeliverySeam { state_dir, hook }
    }

    pub(crate) fn fire_member_delivery(state_dir: &Path) {
        let hook = MEMBER_DELIVERY
            .lock()
            .ok()
            .and_then(|hooks| hooks.get(state_dir).cloned());
        if let Some(hook) = hook {
            hook();
        }
    }

    type ResponseHook = Arc<dyn Fn(&mut serde_json::Value) + Send + Sync>;
    static MEMBER_JOB_LIST: LazyLock<Mutex<BTreeMap<PathBuf, ResponseHook>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));

    /// RAII registration that observes/mutates the native
    /// `member_job_list` response after the real handler ran. Used only to
    /// simulate an older daemon on the wire (e.g. strip the scope ack); it
    /// cannot fabricate a principal or projection.
    pub struct MemberJobListSeam {
        state_dir: PathBuf,
        hook: ResponseHook,
    }

    impl Drop for MemberJobListSeam {
        fn drop(&mut self) {
            let Ok(mut hooks) = MEMBER_JOB_LIST.lock() else {
                return;
            };
            let ours = hooks
                .get(&self.state_dir)
                .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook));
            if ours {
                hooks.remove(&self.state_dir);
            }
        }
    }

    pub fn arm_member_job_list_response(state_dir: &Path, hook: ResponseHook) -> MemberJobListSeam {
        let state_dir = state_dir.to_path_buf();
        if let Ok(mut hooks) = MEMBER_JOB_LIST.lock() {
            hooks.insert(state_dir.clone(), Arc::clone(&hook));
        }
        MemberJobListSeam { state_dir, hook }
    }

    pub(super) fn fire_member_job_list(state_dir: &Path, out: &mut serde_json::Value) {
        let hook = MEMBER_JOB_LIST
            .lock()
            .ok()
            .and_then(|hooks| hooks.get(state_dir).cloned());
        if let Some(hook) = hook {
            hook(out);
        }
    }

    /// RAII registration for the post-pending stage. The callback receives
    /// the durable pending receipt and the exact body the writer is about
    /// to render, so a test observes real content without inventing it.
    pub struct PostPendingSeam {
        state_dir: PathBuf,
        hook: PendingHook,
    }

    impl Drop for PostPendingSeam {
        fn drop(&mut self) {
            let Ok(mut hooks) = POST_PENDING.lock() else {
                return;
            };
            let ours = hooks
                .get(&self.state_dir)
                .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook));
            if ours {
                hooks.remove(&self.state_dir);
            }
        }
    }

    /// RAII registration for one publication stage. Drop removes only the
    /// hook it installed in its own registry.
    pub struct PublicationStageSeam {
        registry: &'static LazyLock<Mutex<BTreeMap<PathBuf, Hook>>>,
        state_dir: PathBuf,
        hook: Hook,
    }

    impl Drop for PublicationStageSeam {
        fn drop(&mut self) {
            let Ok(mut hooks) = self.registry.lock() else {
                return;
            };
            let ours = hooks
                .get(&self.state_dir)
                .is_some_and(|hook| Arc::ptr_eq(hook, &self.hook));
            if ours {
                hooks.remove(&self.state_dir);
            }
        }
    }

    fn arm(
        registry: &'static LazyLock<Mutex<BTreeMap<PathBuf, Hook>>>,
        state_dir: &Path,
        hook: Hook,
    ) -> PublicationStageSeam {
        let state_dir = state_dir.to_path_buf();
        if let Ok(mut hooks) = registry.lock() {
            hooks.insert(state_dir.clone(), Arc::clone(&hook));
        }
        PublicationStageSeam {
            registry,
            state_dir,
            hook,
        }
    }

    fn fire_registry(
        registry: &'static LazyLock<Mutex<BTreeMap<PathBuf, Hook>>>,
        state_dir: &Path,
    ) {
        let hook = registry
            .lock()
            .ok()
            .and_then(|hooks| hooks.get(state_dir).cloned());
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Fired by `publish` after the pending receipt is durable and before
    /// it enters the tracker lock/write; no lock is held. The callback
    /// receives the pending receipt and the exact body bytes.
    pub fn arm_post_pending(state_dir: &Path, hook: PendingHook) -> PostPendingSeam {
        let state_dir = state_dir.to_path_buf();
        if let Ok(mut hooks) = POST_PENDING.lock() {
            hooks.insert(state_dir.clone(), Arc::clone(&hook));
        }
        PostPendingSeam { state_dir, hook }
    }

    /// Fired by `reconcile` immediately before it enters the tracker
    /// lock/write; no lock is held (a test can hold the real tracker lock
    /// to force ordering).
    pub fn arm_pre_tracker(state_dir: &Path, hook: Hook) -> PublicationStageSeam {
        arm(&PRE_TRACKER, state_dir, hook)
    }

    pub(super) fn fire_post_pending(
        state_dir: &Path,
        receipt: &super::PublicationReceipt,
        body: &str,
    ) {
        let hook = POST_PENDING
            .lock()
            .ok()
            .and_then(|hooks| hooks.get(state_dir).cloned());
        if let Some(hook) = hook {
            hook(receipt, body);
        }
    }

    pub(super) fn fire_pre_tracker(state_dir: &Path) {
        fire_registry(&PRE_TRACKER, state_dir);
    }
}

#[cfg(all(test, feature = "test-seam"))]
pub use race_seam::{
    arm_member_delivery_post_projection, arm_member_job_list_response, arm_post_pending,
    arm_pre_tracker, arm_publication_race, MemberDeliverySeam, MemberJobListSeam, PostPendingSeam,
    PublicationRaceSeam, PublicationStageSeam,
};

#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn member_delivery_seam_fire(state_dir: &std::path::Path) {
    race_seam::fire_member_delivery(state_dir);
}

#[cfg(all(test, feature = "test-seam"))]
pub(crate) fn member_job_list_seam_fire(state_dir: &std::path::Path, out: &mut serde_json::Value) {
    race_seam::fire_member_job_list(state_dir, out);
}

/// One selected result, bound by its content digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectedResult {
    pub message: String,
    pub digest: String,
}

impl SelectedResult {
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// What the operator asked to publish as a shared issue comment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationRequest {
    pub delivery_id: String,
    pub issue: String,
    pub project: String,
    /// The exact private result message ids. Empty publishes nothing.
    pub results: Vec<String>,
}

/// A proven local operator connection; never constructed from request data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedPublisher {
    _private: (),
}

impl VerifiedPublisher {
    pub(crate) fn operator() -> Self {
        Self { _private: () }
    }

    fn actor(&self) -> String {
        "operator".to_string()
    }
}

/// The state of one publication attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PublicationState {
    /// The shared write was attempted; its outcome is not recorded. A
    /// later attempt refuses until the operator reconciles it.
    Pending,
    /// The shared ticket comment that now carries the approved results.
    Published { comment: String },
}

/// The durable audit record of one publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationReceipt {
    pub delivery_id: String,
    pub issue: String,
    pub project: String,
    /// The exact private result message ids that were shared.
    pub results: Vec<String>,
    /// The verified actor the publication is attributed to.
    pub actor: String,
    pub at: i64,
    /// Digest over the delivery binding, the selected results and the
    /// actor — a later apply re-verifies exactly this snapshot.
    pub digest: String,
    /// Digest of the exact comment body written for this attempt, so a
    /// reconcile can prove the surviving tracker comment is this attempt.
    pub body_digest: String,
    /// Unique id of this attempt. The pending receipt is persisted before
    /// the shared write and settlement names exactly this attempt, so two
    /// concurrent callers cannot both authorize a comment.
    pub attempt: String,
    pub outcome: PublicationState,
}

/// A checked publication, ready to apply. Only [`publish`] may write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationGrant {
    pub delivery_id: String,
    pub issue: String,
    pub project: String,
    pub results: Vec<SelectedResult>,
    pub actor: String,
    pub digest: String,
}

/// The comment author spelling `issue::write::add_comment` accepts. The
/// attribution comes from the typed [`VerifiedPublisher`] — agents and
/// unproven callers have none.
fn comment_author(actor: &str) -> String {
    let raw = actor.strip_prefix("users/").unwrap_or(actor);
    let name: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(64)
        .collect();
    if name.is_empty() {
        "operator".to_string()
    } else {
        name
    }
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn grant_digest(delivery: &PrivateDelivery, results: &[SelectedResult], actor: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cadence.publication-grant/1\0");
    h.update(delivery.id.as_bytes());
    h.update([0]);
    h.update(delivery.origin.job.as_bytes());
    h.update([0]);
    h.update(delivery.origin.task.as_bytes());
    h.update([0]);
    let mut digits = [0u8; 20];
    let mut value = delivery.origin.revision.unsigned_abs();
    let mut start = digits.len();
    loop {
        start -= 1;
        digits[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    if delivery.origin.revision < 0 {
        start -= 1;
        digits[start] = b'-';
    }
    h.update(&digits[start..]);
    h.update([0]);
    h.update(delivery.origin.dispatch.as_bytes());
    h.update([0]);
    h.update(delivery.origin.issue.as_bytes());
    h.update([0]);
    h.update(delivery.origin.project.as_bytes());
    h.update([0]);
    for result in results {
        h.update(result.message.as_bytes());
        h.update([0]);
        h.update(result.digest.as_bytes());
        h.update([0]);
    }
    h.update(actor.as_bytes());
    hex(h.finalize())
}

/// The live project write-permission check publication must pass.
///
/// The local proven operator is the only publication principal, and the
/// same project policy the board's issue-write path consults
/// (`ui/write_path.rs`) admits it for read/contribute. The ticket and
/// project must both exist and agree, so a request cannot name a
/// different project than the delivery is bound to. Any other principal
/// fails closed: there is no verified member write authority yet.
fn write_authority(pm: &Pm, actor: &str, issue: &str, project: &str) -> Result<()> {
    if actor != "operator" {
        return Err(Error::rejected(
            "publication refused: only the proven local operator may publish; no verified \
             member write authority exists",
        ));
    }
    let (recorded, _) = crate::issue::write::issue_dir(pm, issue).map_err(|_| {
        Error::rejected("publication refused: the target ticket is not uniquely resolvable")
    })?;
    if recorded.key != project || !recorded.allows(crate::issue::project::ProjectCaller::Operator) {
        return Err(Error::rejected(
            "publication refused: the project policy does not admit the operator for this write",
        ));
    }
    Ok(())
}

/// Authorize one explicit publication. Nothing is written.
///
/// Authority is the typed publisher plus the live project policy read
/// here — never a request field and never a raw `(org, sub)` claim.
/// Every refusal names what is missing; none is satisfied by a ticket
/// id, a current job, an alias or a file name.
pub fn authorize(
    pm: &Pm,
    delivery: &PrivateDelivery,
    request: &PublicationRequest,
    publisher: &VerifiedPublisher,
) -> Result<PublicationGrant> {
    if delivery.id != request.delivery_id {
        return Err(Error::rejected(
            "publication refused: the named delivery is not the one read",
        ));
    }
    let actor = publisher.actor();
    // The live project write gate: the proven operator, an existing
    // ticket, and the project policy that admits it for issue writes —
    // the same policy the board's write path consults. No read grant is
    // promoted to write authority and no caller-supplied boolean exists.
    write_authority(pm, &actor, &request.issue, &request.project)?;
    if delivery.state != PrivateState::Completed || delivery.results.is_empty() {
        return Err(Error::rejected(
            "publication refused: the private delivery has no durable result to publish",
        ));
    }
    if delivery.origin.issue != request.issue || delivery.origin.project != request.project {
        return Err(Error::rejected(
            "publication refused: a private delivery publishes only to its own bound ticket",
        ));
    }
    if request.results.is_empty() {
        return Err(Error::rejected(
            "publication refused: select the exact result ids to publish — there is no \"all\"",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut selected = Vec::with_capacity(request.results.len());
    for message in &request.results {
        if !seen.insert(message.as_str()) {
            return Err(Error::rejected(format!(
                "publication refused: result {message} is selected twice"
            )));
        }
        let result = delivery.result(message).ok_or_else(|| {
            Error::rejected(format!(
                "publication refused: {message} is not a result of this delivery"
            ))
        })?;
        if result.message != delivery.origin.dispatch || result.revision != delivery.origin.revision
        {
            return Err(Error::rejected(
                "publication refused: the result does not match the delivery's dispatch",
            ));
        }
        if delivery.shared(message) {
            return Err(Error::rejected(format!(
                "publication refused: {message} is already shared or has an unresolved \
                 publication attempt — the operator reconciles it"
            )));
        }
        selected.push(SelectedResult {
            message: message.clone(),
            digest: result.digest.clone(),
        });
    }
    let digest = grant_digest(delivery, &selected, &actor);
    Ok(PublicationGrant {
        delivery_id: delivery.id.clone(),
        issue: delivery.origin.issue.clone(),
        project: delivery.origin.project.clone(),
        results: selected,
        actor,
        digest,
    })
}

/// Apply an authorized grant to the shared ticket.
///
/// Every deciding check runs again inside the ledger lock together with
/// the pending-receipt append, so two concurrent callers cannot both
/// leave authorized. The tracker write is keyed by the attempt and is
/// idempotent; settlement targets exactly this attempt. An uncertain
/// failure leaves the pending receipt for [`reconcile`].
pub fn publish(
    pm: &Pm,
    state_dir: &std::path::Path,
    grant: &PublicationGrant,
) -> Result<PublicationReceipt> {
    let snapshot = private::load(state_dir)?;
    let delivery = snapshot.get(&grant.delivery_id).ok_or_else(|| {
        Error::rejected(format!(
            "publication refused: private delivery {} is not recorded",
            grant.delivery_id
        ))
    })?;
    let attempt = uuid::Uuid::new_v4().simple().to_string();
    let at = crate::issue::time::now_epoch();
    // Body is deterministic for this attempt; the secret guard runs before
    // any receipt exists.
    let body = publication_body(
        delivery,
        &grant.results,
        &grant.actor,
        &attempt,
        &grant.digest,
    );
    let _ = crate::secret::guard(&format!("{}: publication", grant.issue), &body)?;
    let body_digest = body_digest(&body);
    // Test-only interleaving point: after the real stale-read checks and
    // immediately before the ledger lock, with no lock held here.
    #[cfg(all(test, feature = "test-seam"))]
    race_seam::fire(state_dir);
    let pending = private::begin_publication(state_dir, &grant.delivery_id, |delivery| {
        if delivery.state != PrivateState::Completed
            || delivery.origin.issue != grant.issue
            || delivery.origin.project != grant.project
        {
            return Err(Error::rejected(
                "publication refused: the private delivery binding changed since it was \
                 authorized",
            ));
        }
        for selected in &grant.results {
            let Some(result) = delivery.result(&selected.message) else {
                return Err(Error::rejected(format!(
                    "publication refused: result {} disappeared from the delivery",
                    selected.message
                )));
            };
            if result.digest != selected.digest {
                return Err(Error::rejected(format!(
                    "publication refused: result {} changed since it was authorized",
                    selected.message
                )));
            }
            // The stored digest must bind the stored bytes — equality with
            // the copy the grant carried is not enough.
            let recomputed =
                private::result_digest(&result.message, result.sha.as_deref(), &result.body);
            if recomputed != selected.digest {
                return Err(Error::rejected(format!(
                    "publication refused: result {} digest does not match its stored bytes",
                    selected.message
                )));
            }
            if delivery.shared(&selected.message) {
                return Err(Error::rejected(format!(
                    "publication refused: result {} is already shared or pending — the \
                     operator reconciles it",
                    selected.message
                )));
            }
        }
        if grant_digest(delivery, &grant.results, &grant.actor) != grant.digest {
            return Err(Error::rejected(
                "publication refused: the grant does not match the delivery it names",
            ));
        }
        Ok(PublicationReceipt {
            delivery_id: grant.delivery_id.clone(),
            issue: grant.issue.clone(),
            project: grant.project.clone(),
            results: grant
                .results
                .iter()
                .map(SelectedResult::message)
                .map(str::to_string)
                .collect(),
            actor: grant.actor.clone(),
            at,
            digest: grant.digest.clone(),
            body_digest: body_digest.clone(),
            attempt: attempt.clone(),
            outcome: PublicationState::Pending,
        })
    })?;
    // The pending receipt is durable. No ledger lock is held here; the
    // keyed tracker write is exactly-once and safe to retry.
    #[cfg(all(test, feature = "test-seam"))]
    race_seam::fire_post_pending(state_dir, &pending, &body);
    let written = crate::issue::write::add_comment_keyed(
        pm,
        &pending.issue,
        &pending.project,
        &body,
        Some(&comment_author(&pending.actor)),
        Some("publication"),
        pending.at,
        &pending.attempt,
        &pending.actor,
    )?;
    let comment = written["comment"]
        .as_str()
        .filter(|comment| !comment.is_empty())
        .ok_or_else(|| Error::rejected("publication outcome has no committed comment name"))?
        .to_string();
    let published = PublicationReceipt {
        outcome: PublicationState::Published {
            comment: comment.clone(),
        },
        ..pending
    };
    settle_published(state_dir, &grant.delivery_id, published)
}

/// Settle one attempt; if another caller settled the same attempt first,
/// accept their receipt only when the attempt, target, selection, digests,
/// actor, timestamp and committed comment all match. Otherwise preserve
/// the original error (unknown outcome stays unknown).
fn settle_published(
    state_dir: &std::path::Path,
    delivery_id: &str,
    published: PublicationReceipt,
) -> Result<PublicationReceipt> {
    let attempt = published.attempt.as_str();
    match private::settle_publication(state_dir, delivery_id, attempt, published.clone()) {
        Ok(_) => Ok(published),
        Err(error) => {
            let all = private::load(state_dir)?;
            let Some(now) = all.get(delivery_id).and_then(|delivery| {
                delivery
                    .publications
                    .iter()
                    .find(|receipt| receipt.attempt == attempt)
            }) else {
                return Err(error);
            };
            let (
                PublicationState::Published { comment: ours },
                PublicationState::Published { comment: theirs },
            ) = (&published.outcome, &now.outcome)
            else {
                return Err(error);
            };
            if now.delivery_id != published.delivery_id
                || now.issue != published.issue
                || now.project != published.project
                || now.results != published.results
                || now.digest != published.digest
                || now.body_digest != published.body_digest
                || now.actor != published.actor
                || now.at != published.at
                || ours != theirs
            {
                return Err(error);
            }
            Ok(now.clone())
        }
    }
}

/// Operator recovery for one pending publication attempt.
///
/// Re-verifies the receipt against the current delivery bytes, then
/// reverifies or writes the exact keyed comment (committed at HEAD) and
/// settles that attempt. Never guesses absence, never overwrites, never
/// reruns a task; any unprovable outcome leaves the receipt pending.
pub fn reconcile(
    pm: &Pm,
    state_dir: &std::path::Path,
    delivery_id: &str,
    attempt: &str,
) -> Result<Value> {
    let all = private::load(state_dir)?;
    let delivery = all.get(delivery_id).ok_or_else(|| {
        Error::rejected(format!(
            "publication reconcile: {delivery_id} is not recorded"
        ))
    })?;
    let receipt = delivery
        .publications
        .iter()
        .find(|receipt| receipt.attempt == attempt)
        .ok_or_else(|| {
            Error::rejected(format!(
                "publication reconcile: {delivery_id} has no attempt {attempt}"
            ))
        })?;
    if receipt.delivery_id != delivery_id
        || receipt.results.is_empty()
        || receipt.actor != "operator"
        || delivery.state != PrivateState::Completed
        || delivery.origin.issue != receipt.issue
        || delivery.origin.project != receipt.project
    {
        return Err(Error::rejected(
            "publication reconcile refused: the delivery binding changed; operator inspection \
             is required",
        ));
    }
    let mut selected = Vec::with_capacity(receipt.results.len());
    for message in &receipt.results {
        let result = delivery.result(message).ok_or_else(|| {
            Error::rejected(format!(
                "publication reconcile refused: result {message} is gone from the delivery"
            ))
        })?;
        let recomputed =
            private::result_digest(&result.message, result.sha.as_deref(), &result.body);
        if recomputed != result.digest {
            return Err(Error::rejected(format!(
                "publication reconcile refused: result {message} digest does not match its \
                 stored bytes"
            )));
        }
        selected.push(SelectedResult {
            message: message.clone(),
            digest: result.digest.clone(),
        });
    }
    if grant_digest(delivery, &selected, &receipt.actor) != receipt.digest {
        return Err(Error::rejected(
            "publication reconcile refused: the receipt no longer matches its delivery",
        ));
    }
    let body = publication_body(
        delivery,
        &selected,
        &receipt.actor,
        attempt,
        &receipt.digest,
    );
    let expected = body_digest(&body);
    if receipt.body_digest != expected {
        return Err(Error::rejected(
            "publication reconcile refused: the receipt's body digest no longer matches the \
             delivery",
        ));
    }
    if let PublicationState::Published { comment } = &receipt.outcome {
        if comment.is_empty() {
            return Err(Error::rejected(
                "publication receipt has no committed comment name",
            ));
        }
        return Ok(json!({"state": "published", "attempt": attempt,
                         "comment": comment, "published": receipt}));
    }
    #[cfg(all(test, feature = "test-seam"))]
    race_seam::fire_pre_tracker(state_dir);
    let written = crate::issue::write::add_comment_keyed(
        pm,
        &receipt.issue,
        &receipt.project,
        &body,
        Some(&comment_author(&receipt.actor)),
        Some("publication"),
        receipt.at,
        attempt,
        &receipt.actor,
    )?;
    let comment = written["comment"]
        .as_str()
        .filter(|comment| !comment.is_empty())
        .ok_or_else(|| Error::rejected("publication outcome has no committed comment name"))?
        .to_string();
    let published = PublicationReceipt {
        outcome: PublicationState::Published {
            comment: comment.clone(),
        },
        ..receipt.clone()
    };
    let now = settle_published(state_dir, delivery_id, published)?;
    let PublicationState::Published { comment } = &now.outcome else {
        return Err(Error::rejected("publication outcome is not settled"));
    };
    Ok(json!({"state": "published", "attempt": attempt,
               "comment": comment, "published": now}))
}

fn body_digest(body: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cadence.publication-comment/1\0");
    h.update(body.as_bytes());
    hex(h.finalize())
}

/// The shared comment body. Deterministic for one attempt, so the digest
/// and the written text cannot drift.
fn publication_body(
    delivery: &PrivateDelivery,
    results: &[SelectedResult],
    actor: &str,
    attempt: &str,
    digest: &str,
) -> String {
    let origin = &delivery.origin;
    let mut body = format!(
        "Publication-Attempt: {attempt}\nPublication-Digest: {digest}\n\n\
         Private job delivery `{id}` — explicitly published by {actor}.\n\n\
         Job {job} · task {task} · revision {revision} · issue {issue}\n",
        id = delivery.id,
        actor = actor,
        job = origin.job,
        task = origin.task,
        revision = origin.revision,
        issue = origin.issue,
    );
    for selected in results {
        if let Some(result) = delivery.result(&selected.message) {
            body.push_str(&format!(
                "\n## Result {message}\n\n{text}\n",
                message = selected.message,
                text = result.body.trim_end(),
            ));
            if let Some(sha) = result.sha.as_deref() {
                body.push_str(&format!("\nRevision `{sha}`\n"));
            }
        }
    }
    body
}
