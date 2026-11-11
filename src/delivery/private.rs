//! CAD-2215: private job deliveries, recorded beside — never inside —
//! the ordinary review loop.
//!
//! [`super::Record`] is ordinary work: it exists because the daemon
//! accepted a dispatch and its file, `delivery.json`, is read by the
//! master, shared Needs-you rows and every member stream. A member's
//! assistant job (an `assist_requests` row) is that member's own work,
//! so its delivery lives here instead: a separate, versioned envelope
//! keyed by a stable delivery id, whose origin is derived from native
//! rows only.
//!
//! Three invariants hold at every function:
//!
//! - A private origin is never caller-supplied. [`VerifiedJobOrigin::derive`]
//!   accepts exact `Job`/`Task`/`AssistRequest` rows the store returned;
//!   no ticket id, current active job, worker alias or file name is ever
//!   read as provenance.
//! - No function in this module writes `delivery.json`. A private
//!   delivery therefore cannot replace, suppress or mutate ordinary
//!   state, even when both name the same ticket.
//! - Readers are limited to the delivery's exact owner and the operator.
//!   "Nothing" is the answer for everyone else — no row, count or id.
//!
//! This file is not an isolation proof. It is daemon-owned state read by
//! daemon projections; the same-uid host boundary is separate and
//! `private_available()` stays `false` until that boundary exists.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::publication::{PublicationReceipt, PublicationState};
use crate::error::{Error, Result};
use crate::store::{AssistRequest, Job, Task};

/// The private ledger, beside the ordinary loop's `delivery.json`.
pub const FILE: &str = "delivery-private.json";
/// Envelope version. A reader that does not know a version refuses the
/// file rather than interpreting it.
const VERSION: u32 = 1;

/// Serializes this process's read-modify-write passes. The daemon is the
/// only writer of this file; the ordinary `delivery_lock` is not used
/// here on purpose, so a private write can never delay or fail an
/// ordinary loop transaction.
static LEDGER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The stable id of one private delivery: a job task, its revision and
/// the EXACT accepted dispatch message. A reopened task can reuse a
/// revision with a new deterministic kickoff id, so `(job, task,
/// revision)` alone collides; the dispatch message is the origin.
pub fn delivery_id(job: &str, task: &str, revision: i64, dispatch: &str) -> String {
    format!("job:{job}:task:{task}:rev:{revision}:dispatch:{dispatch}")
}

/// A job/task/owner binding derived from native rows. Nothing here comes
/// from a request payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedJobOrigin {
    pub job: String,
    pub task: String,
    pub revision: i64,
    /// The exact dispatch message this delivery follows. Required: an
    /// undispatched draft has no private delivery.
    pub dispatch: String,
    pub issue: String,
    pub project: String,
    pub owner_org: String,
    pub owner_sub: String,
    /// The authority revision the member's request committed under, when
    /// the store recorded one. Evidence only — a later mint does not
    /// rewrite it.
    #[serde(default)]
    pub authority_rev: Option<String>,
}

impl VerifiedJobOrigin {
    /// Derive the one accepted origin from rows the store returned.
    ///
    /// `request` must be the job's unique `assist_requests` row — the
    /// caller reads it with `assist_request_for_job`, which answers
    /// `None` when the job has more than one owner. Any disagreement
    /// between the three rows is a refusal, never a guess.
    pub fn derive(job: &Job, task: &Task, request: &AssistRequest, project: &str) -> Result<Self> {
        if task.job_id != job.id || request.job != job.id || task.revision <= 0 {
            return Err(Error::rejected(
                "private delivery origin: the task, revision and request do not name this dispatch",
            ));
        }
        let issue = job
            .issue_id
            .as_deref()
            .filter(|i| !i.is_empty() && *i == i.trim());
        let Some(issue) = issue else {
            return Err(Error::rejected(
                "private delivery origin: the job is not bound to a tracker issue",
            ));
        };
        if project.trim().is_empty() {
            return Err(Error::rejected(
                "private delivery origin: the ticket's project did not resolve",
            ));
        }
        if request.org.trim().is_empty() || request.sub.trim().is_empty() {
            return Err(Error::rejected(
                "private delivery origin: the request carries no recorded owner",
            ));
        }
        let dispatch = task
            .dispatch_message
            .as_deref()
            .filter(|dispatch| !dispatch.trim().is_empty())
            .ok_or_else(|| {
                Error::rejected(
                    "private delivery origin: the task has no accepted dispatch message",
                )
            })?;
        if dispatch != dispatch.trim() {
            return Err(Error::rejected(
                "private delivery origin: the dispatch pointer is not a canonical message id",
            ));
        }
        Ok(Self {
            job: job.id.clone(),
            task: task.id.clone(),
            revision: task.revision,
            dispatch: dispatch.to_string(),
            issue: issue.to_string(),
            project: project.to_string(),
            owner_org: request.org.clone(),
            owner_sub: request.sub.clone(),
            authority_rev: request.authority_rev.clone(),
        })
    }

    pub fn delivery_id(&self) -> String {
        delivery_id(&self.job, &self.task, self.revision, &self.dispatch)
    }

    /// Build the origin of a PROVEN historical dispatch whose task row has
    /// since moved to another dispatch. Every argument is evidence read
    /// from the durable dispatch/report rows; the current task's
    /// `revision`/`assignee`/`head_sha` are never consulted.
    #[allow(clippy::too_many_arguments)]
    pub fn for_dispatch(
        job: &Job,
        task: &str,
        revision: i64,
        dispatch: &str,
        issue: &str,
        request: &AssistRequest,
        project: &str,
    ) -> Result<Self> {
        if task.trim().is_empty()
            || dispatch.trim().is_empty()
            || dispatch != dispatch.trim()
            || issue.trim().is_empty()
            || job.issue_id.as_deref() != Some(issue)
            || revision <= 0
        {
            return Err(Error::rejected(
                "private delivery origin: the historical dispatch evidence is incomplete",
            ));
        }
        if project.trim().is_empty() {
            return Err(Error::rejected(
                "private delivery origin: the ticket's project did not resolve",
            ));
        }
        if request.job != job.id || request.org.trim().is_empty() || request.sub.trim().is_empty() {
            return Err(Error::rejected(
                "private delivery origin: the request does not bind this job's owner",
            ));
        }
        Ok(Self {
            job: job.id.clone(),
            task: task.to_string(),
            revision,
            dispatch: dispatch.to_string(),
            issue: issue.to_string(),
            project: project.to_string(),
            owner_org: request.org.clone(),
            owner_sub: request.sub.clone(),
            authority_rev: request.authority_rev.clone(),
        })
    }
}

/// Where a private delivery is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateState {
    /// Dispatched; no durable result yet.
    Running,
    /// A durable result was recorded.
    Completed,
    /// The task failed or was cancelled by its own recorded path.
    Failed,
    /// The outcome could not be settled; operator reconciliation only.
    Unknown,
}

impl PrivateState {
    pub fn as_str(self) -> &'static str {
        match self {
            PrivateState::Running => "running",
            PrivateState::Completed => "completed",
            PrivateState::Failed => "failed",
            PrivateState::Unknown => "unknown",
        }
    }

    pub fn terminal(self) -> bool {
        matches!(
            self,
            PrivateState::Completed | PrivateState::Failed | PrivateState::Unknown
        )
    }
}

/// One durable result — a task row's recorded outcome for the exact
/// dispatch message that carried the work.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivateResult {
    /// The native dispatch message id this result belongs to.
    pub message: String,
    pub revision: i64,
    #[serde(default)]
    pub sha: Option<String>,
    /// The result text the daemon recorded, verbatim.
    pub body: String,
    /// Digest over `message`, `sha` and `body` — binds a publication
    /// grant to exactly the content that was authorized.
    pub digest: String,
    pub at: i64,
}

/// Digest of one result's identity and content.
pub fn result_digest(message: &str, sha: Option<&str>, body: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"cadence.private-result/1\0");
    h.update(message.as_bytes());
    h.update([0]);
    h.update(sha.unwrap_or_default().as_bytes());
    h.update([0]);
    h.update(body.as_bytes());
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = h.finalize();
    let mut digest = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        digest.push(HEX[(byte >> 4) as usize] as char);
        digest.push(HEX[(byte & 0x0f) as usize] as char);
    }
    digest
}

/// One private job delivery. `id` is [`delivery_id`] and is stable for
/// the whole task revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PrivateDelivery {
    pub id: String,
    pub origin: VerifiedJobOrigin,
    pub state: PrivateState,
    pub opened_at: i64,
    pub results: Vec<PrivateResult>,
    /// Explicit publications of selected results. Empty means nothing of
    /// this delivery has ever been shared.
    pub publications: Vec<PublicationReceipt>,
}

impl PrivateDelivery {
    pub fn result(&self, message: &str) -> Option<&PrivateResult> {
        self.results.iter().find(|r| r.message == message)
    }

    /// Whether any receipt — pending or published — already names this
    /// result. A pending receipt is an unknown outcome: it is not a
    /// success and must not be replayed.
    pub fn shared(&self, message: &str) -> bool {
        self.publications
            .iter()
            .any(|p| p.results.iter().any(|id| id == message))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("private delivery serializes")
    }
}

/// Who is asking for a private delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reader<'a> {
    /// The proven operator connection.
    Operator,
    /// The member whose request created the job.
    Owner { org: &'a str, sub: &'a str },
    /// Anyone else, including an agent and an unproven caller.
    Other,
}

/// The one read rule: the exact owner, or the operator.
pub fn may_read(delivery: &PrivateDelivery, reader: Reader<'_>) -> bool {
    match reader {
        Reader::Operator => true,
        Reader::Owner { org, sub } => {
            delivery.origin.owner_org == org && delivery.origin.owner_sub == sub
        }
        Reader::Other => false,
    }
}

/// The deliveries this reader may see, oldest first. A non-owner gets an
/// empty list — never a redacted row, count or id.
pub fn readable<'a>(
    all: &'a BTreeMap<String, PrivateDelivery>,
    reader: Reader<'_>,
) -> Vec<&'a PrivateDelivery> {
    let mut rows: Vec<&PrivateDelivery> = all.values().filter(|d| may_read(d, reader)).collect();
    rows.sort_by(|a, b| a.opened_at.cmp(&b.opened_at).then_with(|| a.id.cmp(&b.id)));
    rows
}

pub fn path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE)
}

/// The whole ledger, for the daemon's writes: a file that exists but does
/// not parse refuses rather than being replaced by an empty map. A
/// symlink or non-regular file refuses — the read never follows it.
pub fn load(state_dir: &Path) -> Result<BTreeMap<String, PrivateDelivery>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path(state_dir))
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    if !file.metadata()?.is_file() {
        return Err(Error::internal(format!(
            "{FILE} is not a regular file — refusing to read it"
        )));
    }
    let mut text = String::new();
    std::io::BufReader::new(file).read_to_string(&mut text)?;
    let envelope: Envelope = serde_json::from_str(&text)
        .map_err(|e| Error::internal(format!("{FILE} is unreadable: {e}")))?;
    if envelope.version != VERSION {
        return Err(Error::internal(format!(
            "{FILE} has envelope version {}, which this build does not read",
            envelope.version
        )));
    }
    Ok(envelope.records)
}

/// Unique same-directory temp files, so a planted name can never be
/// inherited or removed by this writer.
static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn save(state_dir: &Path, all: &BTreeMap<String, PrivateDelivery>) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let file = path(state_dir);
    let unique = format!(
        ".{FILE}.{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let tmp = state_dir.join(unique);
    // Only an inode this call actually created may be cleaned up. A
    // planted temp makes the open fail before this is set.
    let mut owned: Option<(u64, u64)> = None;
    let result = (|| -> Result<()> {
        // A fresh inode only: create_new + O_NOFOLLOW refuses a planted
        // temp, and 0600 plus an explicit chmod overrides the umask and
        // any prior mode once this inode is renamed into place.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        let meta = f.metadata()?;
        owned = Some((meta.dev(), meta.ino()));
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        // Serialize a borrowed view: save never clones the whole ledger.
        serde_json::to_writer_pretty(
            &mut f,
            &EnvelopeRef {
                version: VERSION,
                records: all,
            },
        )?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &file)?;
        std::fs::File::open(state_dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        if let Some((dev, ino)) = owned {
            // Unlink only when the name still resolves to the exact
            // regular inode this call created; a substituted file or
            // symlink is left alone.
            if let Ok(meta) = std::fs::symlink_metadata(&tmp) {
                if meta.file_type().is_file() && meta.dev() == dev && meta.ino() == ino {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        }
    }
    result
}

/// Open (or idempotently return) the delivery for one native origin.
///
/// An existing record at the id must carry the identical origin; a
/// disagreement means two owners or two issues share a delivery id and
/// is a refusal. Replaying the same dispatch returns the stored record
/// unchanged — opening is never a second write of results.
pub fn open(state_dir: &Path, origin: VerifiedJobOrigin, at: i64) -> Result<PrivateDelivery> {
    let _g = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(state_dir)?;
    let id = origin.delivery_id();
    if let Some(existing) = all.get(&id) {
        if existing.origin != origin {
            return Err(Error::rejected(format!(
                "private delivery {id} is already bound to a different origin — operator \
                 reconciliation is required"
            )));
        }
        return Ok(existing.clone());
    }
    let delivery = PrivateDelivery {
        id: id.clone(),
        origin,
        state: PrivateState::Running,
        opened_at: at,
        results: Vec::new(),
        publications: Vec::new(),
    };
    all.insert(id, delivery.clone());
    save(state_dir, &all)?;
    Ok(delivery)
}

/// Record one durable result. The result must belong to this delivery's
/// exact dispatch revision; a replayed result is idempotent, and a second
/// different body for the same message is a refusal, never an overwrite.
pub fn append_result(state_dir: &Path, id: &str, result: PrivateResult) -> Result<PrivateDelivery> {
    let _g = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(state_dir)?;
    let delivery = all.get_mut(id).ok_or_else(|| {
        Error::rejected(format!(
            "private delivery {id} is not recorded — the dispatch receipt is missing"
        ))
    })?;
    if result.revision != delivery.origin.revision {
        return Err(Error::rejected(format!(
            "private delivery {id}: result revision {} does not match the recorded \
             dispatch revision {}",
            result.revision, delivery.origin.revision
        )));
    }
    if result.message != delivery.origin.dispatch {
        return Err(Error::rejected(format!(
            "private delivery {id}: result {} is not the exact recorded dispatch message {}",
            result.message, delivery.origin.dispatch
        )));
    }
    if let Some(existing) = delivery.result(&result.message) {
        if existing.digest != result.digest {
            return Err(Error::rejected(format!(
                "private delivery {id}: message {} already carries a different result — \
                 operator reconciliation is required",
                result.message
            )));
        }
        return Ok(delivery.clone());
    }
    delivery.results.push(result);
    delivery.state = PrivateState::Completed;
    let out = delivery.clone();
    save(state_dir, &all)?;
    Ok(out)
}

/// Move the delivery to a settled non-result state (`failed`, `unknown`).
/// `completed` is only ever reached by [`append_result`]; a caller cannot
/// declare success without a durable result.
pub fn set_state(state_dir: &Path, id: &str, state: PrivateState) -> Result<PrivateDelivery> {
    if state == PrivateState::Completed {
        return Err(Error::rejected(
            "private delivery completion needs a durable result, not a state write",
        ));
    }
    let _g = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(state_dir)?;
    let delivery = all
        .get_mut(id)
        .ok_or_else(|| Error::rejected(format!("private delivery {id} is not recorded")))?;
    // A delivery that already carries a durable result never loses it to
    // a later state write; the operator reconciles a conflicting outcome.
    if delivery.state == PrivateState::Completed {
        return Err(Error::rejected(format!(
            "private delivery {id} already carries a durable result — a state write cannot \
             discard it"
        )));
    }
    delivery.state = state;
    let out = delivery.clone();
    save(state_dir, &all)?;
    Ok(out)
}

/// Begin exactly one publication under the ledger lock.
///
/// `make` runs inside the lock over the current record and must return
/// the pending receipt — every check that decides whether this caller may
/// share (state, binding, selected digests, already-shared/pending, grant
/// digest) therefore decides at most once for concurrent callers. The
/// pending receipt is persisted before this returns, so a crash leaves an
/// inspectable unknown outcome rather than a second writer. No lock is
/// held after return; the caller performs the shared write and then
/// settles the exact attempt.
pub fn begin_publication<F>(state_dir: &Path, id: &str, make: F) -> Result<PublicationReceipt>
where
    F: FnOnce(&PrivateDelivery) -> Result<PublicationReceipt>,
{
    let _g = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(state_dir)?;
    let delivery = all
        .get(id)
        .ok_or_else(|| Error::rejected(format!("private delivery {id} is not recorded")))?;
    let receipt = make(delivery)?;
    if receipt.delivery_id != id {
        return Err(Error::rejected(
            "publication receipt names a different delivery",
        ));
    }
    if receipt.attempt.trim().is_empty() {
        return Err(Error::rejected(
            "publication receipt needs a unique attempt id",
        ));
    }
    if delivery
        .publications
        .iter()
        .any(|p| p.attempt == receipt.attempt)
    {
        return Err(Error::rejected(
            "publication attempt id is already recorded",
        ));
    }
    let delivery = all
        .get_mut(id)
        .ok_or_else(|| Error::rejected(format!("private delivery {id} is not recorded")))?;
    delivery.publications.push(receipt.clone());
    save(state_dir, &all)?;
    Ok(receipt)
}

/// Settle exactly the pending receipt named by `attempt`.
///
/// A missing attempt, a duplicate attempt id or a receipt that is no
/// longer pending is a refusal — settlement never guesses which receipt
/// the shared write belongs to. Uncertain failures leave it pending.
pub fn settle_publication(
    state_dir: &Path,
    id: &str,
    attempt: &str,
    receipt: PublicationReceipt,
) -> Result<PrivateDelivery> {
    if receipt.delivery_id != id || receipt.attempt != attempt {
        return Err(Error::rejected(
            "publication receipt does not name the attempt being settled",
        ));
    }
    let _g = LEDGER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = load(state_dir)?;
    let delivery = all
        .get_mut(id)
        .ok_or_else(|| Error::rejected(format!("private delivery {id} is not recorded")))?;
    let mut slots = delivery
        .publications
        .iter_mut()
        .filter(|p| p.attempt == attempt);
    let Some(slot) = slots.next() else {
        return Err(Error::rejected(format!(
            "private delivery {id}: no pending publication with attempt {attempt}"
        )));
    };
    if slots.next().is_some() {
        return Err(Error::rejected(format!(
            "private delivery {id}: attempt {attempt} is ambiguous — operator reconciliation \
             is required"
        )));
    }
    if !matches!(slot.outcome, PublicationState::Pending) {
        return Err(Error::rejected(format!(
            "private delivery {id}: attempt {attempt} is not pending"
        )));
    }
    *slot = receipt;
    let out = delivery.clone();
    save(state_dir, &all)?;
    Ok(out)
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    records: BTreeMap<String, PrivateDelivery>,
}

/// The same envelope shape, serialized from a borrowed ledger so `save`
/// never clones the records it is about to write.
#[derive(Serialize)]
struct EnvelopeRef<'a> {
    version: u32,
    records: &'a BTreeMap<String, PrivateDelivery>,
}
