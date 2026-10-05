//! Positive factories are scoped to the REAL root-owned operation, not a
//! child-supplied context. Cross-UID /proc access is not assumed or simulated.
use super::super::{refused, Deadline, QualifiedImage, Result};
use super::{channel, child, custody, pin, wire, QualifiedBootstrap};
use crate::daemon::installer_enrollment_wire as receipt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

pub(super) struct Pair {
    pub installer: child::HeldChild,
    pub recipient: child::HeldChild,
}
struct Provider {
    channel: channel::Duplex,
    sequence: u64,
    consumed: bool,
    last_wall: u64,
}
struct Handoff {
    released: bool,
    completed: bool,
}
struct Context {
    bootstrap: QualifiedBootstrap,
    root: Arc<custody::RootCustody>,
    pair: Arc<Mutex<Pair>>,
    installer: wire::Installer,
    recipient: wire::Recipient,
    binding: String,
    receipt_keys: Vec<receipt::TrustedKey>,
    grant_keys: Vec<&'static [u8]>,
    provider: Mutex<Provider>,
    installer_channel: Mutex<channel::Duplex>,
    recipient_channel: Mutex<channel::Duplex>,
    deadline: Deadline,
    last_wall: AtomicU64,
    handoff: Mutex<Handoff>,
}
static CONTEXT: OnceLock<Context> = OnceLock::new();
fn context() -> Result<&'static Context> {
    CONTEXT.get().ok_or_else(refused)
}
pub(crate) fn has_context() -> bool {
    CONTEXT.get().is_some()
}
pub(crate) fn receipt_keys() -> Result<&'static [receipt::TrustedKey]> {
    Ok(&context()?.receipt_keys)
}
pub(crate) fn grant_keys() -> Result<&'static [&'static [u8]]> {
    Ok(&context()?.grant_keys)
}
pub(crate) fn image() -> Result<QualifiedImage> {
    let c = context()?;
    c.check(c.deadline)?;
    Ok(c.root.image.clone())
}
pub(crate) fn binding_json() -> Result<&'static str> {
    Ok(&context()?.binding)
}
pub(crate) fn enrollment() -> Result<(u32, u64, String, [u8; 32])> {
    let c = context()?;
    Ok((
        c.recipient.pid,
        c.recipient.starttime.parse().map_err(|_| refused())?,
        c.recipient.generation.clone(),
        pin(&c.bootstrap.manifest.artifacts.supervisor)?,
    ))
}
pub(crate) fn installer() -> Result<crate::daemon::InstallerProcessRecord> {
    let i = &context()?.installer;
    Ok(crate::daemon::InstallerProcessRecord {
        pid: i.pid,
        starttime: i.starttime.clone(),
        uid: i.uid,
        gid: i.gid,
        client_digest: i.client_digest.clone(),
        carrier_digest: i.carrier_digest.clone(),
        observer_digest: i.observer_digest.clone(),
    })
}
pub(super) fn runtime_ms() -> Result<u64> {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| refused())?
            .as_millis(),
    )
    .map_err(|_| refused())
}
impl Context {
    fn check(&self, deadline: Deadline) -> Result<()> {
        deadline.check()?;
        self.deadline.check()?;
        self.root.recheck(deadline)?;
        let now = runtime_ms()?;
        if now < self.last_wall.fetch_max(now, Ordering::SeqCst) {
            return Err(refused());
        }
        if now >= self.bootstrap.expires_at_ms || now >= self.bootstrap.manifest.expires_at_ms {
            return Err(refused());
        }
        let mut pair = self.pair.lock().map_err(|_| refused())?;
        pair.installer.recheck(deadline)?;
        pair.recipient.recheck(deadline)
    }
}
/// Opaque origin proof retaining actual own-created processes. No JSON, PID,
/// file or signature constructor exists. A stale factory clone cannot survive
/// child exit, a second exec, deadline or root-custody loss.
#[derive(Clone)]
pub(crate) struct Proof {
    root: Arc<custody::RootCustody>,
    pair: Arc<Mutex<Pair>>,
    deadline: Deadline,
}
impl Proof {
    pub(crate) fn recheck(&self, until: Instant) -> Result<()> {
        let deadline = Deadline(until.min(self.deadline.0));
        self.root.recheck(deadline)?;
        let mut pair = self.pair.lock().map_err(|_| refused())?;
        pair.installer.recheck(deadline)?;
        pair.recipient.recheck(deadline)
    }
}
pub(crate) fn proof() -> Result<Proof> {
    let c = context()?;
    c.check(c.deadline)?;
    Ok(Proof {
        root: c.root.clone(),
        pair: c.pair.clone(),
        deadline: c.deadline,
    })
}
pub(crate) fn require_self(until: Instant) -> Result<()> {
    context()?.check(Deadline(until))
}
pub(crate) fn custody_recheck(until: Instant) -> Result<()> {
    proof()?.recheck(until)
}
pub(crate) fn owner_request(
    kind: wire::Kind,
    binding: &[u8],
    until: Instant,
) -> Result<wire::OwnerResponse> {
    let c = context()?;
    let deadline = Deadline(until.min(c.deadline.0));
    c.check(deadline)?;
    if binding != c.binding.as_bytes() {
        return Err(refused());
    }
    let mut p = c.provider.lock().map_err(|_| refused())?;
    if kind == wire::Kind::Consume && p.consumed {
        return Err(refused());
    }
    if kind == wire::Kind::ConsumedCurrent && !p.consumed {
        return Err(refused());
    }
    if kind == wire::Kind::Prepared && p.consumed {
        return Err(refused());
    }
    let now = runtime_ms()?;
    if now < p.last_wall {
        return Err(refused());
    }
    p.last_wall = now;
    p.sequence = p.sequence.checked_add(1).ok_or_else(refused)?;
    let sequence = p.sequence;
    if kind == wire::Kind::Consume {
        p.consumed = true;
    } // burn BEFORE sending; ACK loss is UNKNOWN, never retry
    p.channel.send(&wire::json(&serde_json::json!({"version":1,"type":kind,
        "operation":c.bootstrap.operation,"barrierNonce":c.bootstrap.barrier_nonce,"sequence":sequence}))?)?;
    let frame = p.channel.receive()?;
    c.check(deadline)?;
    let now = runtime_ms()?;
    if now < p.last_wall {
        return Err(refused());
    }
    p.last_wall = now;
    let response: wire::OwnerResponse = serde_json::from_slice(&frame.0).map_err(|_| refused())?;
    response.validate(&c.bootstrap, &c.binding, sequence, kind)?;
    Ok(response)
}
pub(super) struct Publication {
    pub bootstrap: QualifiedBootstrap,
    pub root: custody::RootCustody,
    pub pair: Pair,
    pub installer: wire::Installer,
    pub recipient: wire::Recipient,
    pub binding: String,
    pub provider: channel::Duplex,
    pub installer_channel: channel::Duplex,
    pub recipient_channel: channel::Duplex,
    pub deadline: Deadline,
}
pub(super) fn publish(input: Publication) -> Result<()> {
    let Publication {
        bootstrap,
        root,
        pair,
        installer,
        recipient,
        binding,
        provider,
        installer_channel,
        recipient_channel,
        deadline,
    } = input;
    let initial_wall = bootstrap.authenticated_at_ms;
    let receipt_keys = receipt::keys_from_qualified(&bootstrap)?;
    let grant_keys = bootstrap
        .grant_public_keys()?
        .into_iter()
        .map(|key| &*Box::leak(Box::new(key)) as &'static [u8])
        .collect();
    CONTEXT
        .set(Context {
            bootstrap,
            root: Arc::new(root),
            pair: Arc::new(Mutex::new(pair)),
            installer,
            recipient,
            binding,
            receipt_keys,
            grant_keys,
            provider: Mutex::new(Provider {
                channel: provider,
                sequence: 0,
                consumed: false,
                last_wall: runtime_ms()?,
            }),
            installer_channel: Mutex::new(installer_channel),
            recipient_channel: Mutex::new(recipient_channel),
            deadline,
            last_wall: AtomicU64::new(initial_wall),
            handoff: Mutex::new(Handoff {
                released: false,
                completed: false,
            }),
        })
        .map_err(|_| refused())?;
    context()?.check(deadline)
}
fn child_exchange(
    channel: &Mutex<channel::Duplex>,
    message: &wire::ChildReply,
    deadline: Deadline,
) -> Result<wire::ChildRequest> {
    let c = context()?;
    c.check(deadline)?;
    let mut ch = channel.lock().map_err(|_| refused())?;
    ch.send(&wire::json(message)?)?;
    let frame = ch.receive()?;
    c.check(deadline)?;
    serde_json::from_slice(&frame.0).map_err(|_| refused())
}
pub(crate) fn install_once(
    frame: &[u8],
    until: Instant,
    mut current: impl FnMut() -> Result<()>,
) -> Result<()> {
    let c = context()?;
    let deadline = Deadline(until.min(c.deadline.0));
    {
        let mut h = c.handoff.lock().map_err(|_| refused())?;
        if h.released {
            return Err(refused());
        }
        h.released = true;
    }
    let frame = String::from_utf8(frame.to_vec()).map_err(|_| refused())?;
    current()?;
    match child_exchange(
        &c.installer_channel,
        &wire::ChildReply::Release {
            frame: frame.clone(),
        },
        deadline,
    )? {
        wire::ChildRequest::Install { frame: echo } if echo == frame => {}
        _ => return Err(refused()),
    }
    current()?;
    current()?;
    match child_exchange(
        &c.recipient_channel,
        &wire::ChildReply::Install { frame },
        deadline,
    )? {
        wire::ChildRequest::Ready => {}
        _ => return Err(refused()),
    }
    current()
}
pub(crate) fn acknowledge_children(
    ack: &[u8],
    until: Instant,
    mut current: impl FnMut() -> Result<()>,
) -> Result<()> {
    let c = context()?;
    let deadline = Deadline(until.min(c.deadline.0));
    let frame = String::from_utf8(ack.to_vec()).map_err(|_| refused())?;
    current()?;
    match child_exchange(
        &c.recipient_channel,
        &wire::ChildReply::Consumed {
            frame: frame.clone(),
        },
        deadline,
    )? {
        wire::ChildRequest::Ack { frame: echo } if echo == frame => {}
        _ => return Err(refused()),
    }
    current()?;
    current()?;
    match child_exchange(
        &c.installer_channel,
        &wire::ChildReply::Installed { frame },
        deadline,
    )? {
        wire::ChildRequest::Complete => {}
        _ => return Err(refused()),
    }
    current()?;
    c.handoff.lock().map_err(|_| refused())?.completed = true;
    Ok(())
}
pub(super) fn runtime_parts() -> Result<(
    &'static QualifiedBootstrap,
    Arc<custody::RootCustody>,
    &'static str,
    &'static str,
)> {
    let c = context()?;
    if !c.handoff.lock().map_err(|_| refused())?.completed
        || !c.provider.lock().map_err(|_| refused())?.consumed
    {
        return Err(refused());
    }
    Ok((
        &c.bootstrap,
        c.root.clone(),
        &c.binding,
        &c.recipient.generation,
    ))
}
pub(super) fn runtime_wire<T>(f: impl FnOnce(&mut channel::Duplex) -> Result<T>) -> Result<T> {
    f(&mut context()?.provider.lock().map_err(|_| refused())?.channel)
}
pub(super) fn finish() -> Result<()> {
    let c = context()?;
    c.check(c.deadline)?;
    if !c.handoff.lock().map_err(|_| refused())?.completed {
        return Err(refused());
    }
    let mut provider = c.provider.lock().map_err(|_| refused())?;
    if !provider.consumed {
        return Err(refused());
    }
    provider.channel.send(&wire::json(&serde_json::json!({
        "version":1,"type":"ack","operation":c.bootstrap.operation,
        "barrierNonce":c.bootstrap.barrier_nonce,"recipientGeneration":c.recipient.generation}))?)?;
    // Retain children during the platform post-ACK owner check. EOF/cancel is
    // physical cleanup only; never claims durable external retirement.
    drop(provider);
    super::lifecycle::enter(c.deadline)
    // runtime's one cleanup guard supplies the pair's total <=2s budget
}
pub(super) fn cleanup() {
    if let Some(c) = CONTEXT.get() {
        if let Ok(mut pair) = c.pair.lock() {
            pair.installer.cleanup();
            pair.recipient.cleanup();
        }
    }
}
