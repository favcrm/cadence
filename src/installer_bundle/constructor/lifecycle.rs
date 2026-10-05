//! Separately authenticated runtime authority. Enrollment consumption and an
//! expired construction receipt cannot elect this scope or renew its lifetime.
use super::super::{refused, Deadline, Result};
use super::{
    canonical, context, custody, decode, identifier, wire, Header, QualifiedBootstrap, MAX_SAFE,
};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
const DOMAIN: &[u8] = b"cadence.protected-runtime-owner.v1\0";
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Release {
    version: u8,
    #[serde(rename = "type")]
    kind: String,
    operation: String,
    barrier_nonce: String,
    authorization: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Authorization {
    version: u8,
    binding_json: String,
    runtime_generation: String,
    issued_at_ms: u64,
    expires_at_ms: u64,
}
/// The only creator is the real compact-signature guard below. No serialized
/// keys, boolean, lease, marker or caller diagnostic is a constructor.
struct Authenticated {
    until: Instant,
    expires_at_ms: u64,
}
fn authenticate(
    release: &Release,
    b: &QualifiedBootstrap,
    binding: &str,
    generation: &str,
    now: u64,
) -> Result<Authenticated> {
    if release.version != 1
        || release.kind != "runtime-release"
        || release.operation != b.operation
        || release.barrier_nonce != b.barrier_nonce
        || release.authorization.len() > 32768
    {
        return Err(refused());
    }
    let parts: Vec<_> = release.authorization.split('.').collect();
    if parts.len() != 3 {
        return Err(refused());
    }
    let header: Header = canonical(&decode(parts[0], 256)?)?;
    let body: Authorization = canonical(&decode(parts[1], 20000)?)?;
    let signature = decode(parts[2], 64)?;
    if header.alg != "Ed25519"
        || header.issuer != "agenticos-native-owner"
        || header.kind != "protected-runtime-owner"
        || header.version != 1
        || !identifier(&header.kid)
        || body.version != 1
        || body.binding_json != binding
        || body.runtime_generation != generation
        || body.issued_at_ms > now
        || body.expires_at_ms <= now
        || body.expires_at_ms > MAX_SAFE
        || body.expires_at_ms > b.manifest.expires_at_ms
        || body.expires_at_ms - body.issued_at_ms > 86400000
        || signature.len() != 64
    {
        return Err(refused());
    }
    let key = b
        .manifest
        .runtime_trust
        .as_ref()
        .ok_or_else(refused)?
        .iter()
        .find(|k| {
            k.issuer == header.issuer && k.kid == header.kid && k.key_version == header.key_version
        })
        .ok_or_else(refused)?;
    let mut bytes = DOMAIN.to_vec();
    bytes.extend_from_slice(parts[0].as_bytes());
    bytes.push(b'.');
    bytes.extend_from_slice(parts[1].as_bytes());
    ring::signature::UnparsedPublicKey::new(
        &ring::signature::ED25519,
        decode(&key.public_key, 32)?,
    )
    .verify(&bytes, &signature)
    .map_err(|_| refused())?;
    Ok(Authenticated {
        until: Instant::now() + Duration::from_millis(body.expires_at_ms - now),
        expires_at_ms: body.expires_at_ms,
    })
}
struct State {
    sequence: u64,
    last_wall: u64,
}
struct Runtime {
    root: Arc<custody::RootCustody>,
    auth: Authenticated,
    operation: String,
    nonce: String,
    binding: String,
    state: Mutex<State>,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
fn active() -> Result<&'static Runtime> {
    RUNTIME.get().ok_or_else(refused)
}
impl Runtime {
    fn check(&self, until: Instant) -> Result<()> {
        let deadline = Deadline(until.min(self.auth.until));
        deadline.check()?;
        self.root.recheck(deadline)?;
        let now = context::runtime_ms()?;
        let mut state = self.state.lock().map_err(|_| refused())?;
        if now < state.last_wall || now >= self.auth.expires_at_ms {
            return Err(refused());
        }
        state.last_wall = now;
        Ok(())
    }
}
/// Physical root provenance plus separately verified runtime lifetime, NOT an
/// owned helper proof. Dispatch must additionally match its actual child handle.
#[derive(Clone)]
pub(crate) struct RuntimeProof {
    root: Arc<custody::RootCustody>,
    until: Instant,
}
impl RuntimeProof {
    pub(crate) fn recheck(&self, until: Instant) -> Result<()> {
        let r = active()?;
        if !Arc::ptr_eq(&self.root, &r.root) {
            return Err(refused());
        }
        r.check(until.min(self.until))
    }
}
pub(crate) fn runtime_proof(until: Instant) -> Result<RuntimeProof> {
    let r = active()?;
    r.check(until)?;
    Ok(RuntimeProof {
        root: r.root.clone(),
        until: r.auth.until,
    })
}
static PI_KEYS: OnceLock<Vec<super::PiKeyRecord>> = OnceLock::new();
/// Only the real qualified root context and separately authenticated runtime
/// can deliver these read-only PUBLIC keys. No caller key or guest key file.
pub(crate) fn pi_public_keys(until: Instant) -> Result<&'static [super::PiKeyRecord]> {
    let r = active()?;
    r.check(until)?;
    if PI_KEYS.get().is_none() {
        let (bootstrap, _, _, _) = context::runtime_parts()?;
        let ring = bootstrap.manifest.pi_trust.as_ref().ok_or_else(refused)?;
        let keys = ring
            .iter()
            .map(|k| {
                Ok((
                    k.issuer.clone(),
                    k.kid.clone(),
                    k.key_version,
                    decode(&k.public_key, 32)?
                        .try_into()
                        .map_err(|_| refused())?,
                ))
            })
            .collect::<Result<Vec<super::PiKeyRecord>>>()?;
        if keys.is_empty() {
            return Err(refused());
        }
        PI_KEYS.set(keys).map_err(|_| refused())?;
    }
    r.check(until)?;
    Ok(PI_KEYS.get().ok_or_else(refused)?.as_slice())
}
/// Pi signature expiry must be no later than this authenticated runtime/image
/// bound, in addition to its own <=30s operation window and durable currentness.
pub(crate) fn pi_expires_at_ms(until: Instant) -> Result<u64> {
    let r = active()?;
    r.check(until)?;
    Ok(r.auth.expires_at_ms)
}
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum StorePurpose {
    Init,
    Restore,
    Open,
    Close,
    Witness,
}
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum Request<'a> {
    RuntimeCurrent,
    StoreStartup,
    PiAcquire {
        scope: &'a crate::adapter::pi_guest::owner::OperationScope,
    },
    PiConsume {
        reference: &'a str,
        scope: &'a crate::adapter::pi_guest::owner::OperationScope,
    },
    PiCurrent {
        reference: &'a str,
        scope: &'a crate::adapter::pi_guest::owner::OperationScope,
    },
    StoreAcquire {
        purpose: StorePurpose,
        attempt: &'a str,
    },
    StoreConsume {
        reference: &'a str,
    },
    StoreCurrent {
        reference: &'a str,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Response {
    version: u8,
    #[serde(rename = "type")]
    kind: String,
    operation: String,
    barrier_nonce: String,
    sequence: u64,
    lease_expires_at_ms: u64,
    current: wire::OwnerCurrent,
    store: Option<serde_json::Value>,
    pi: Option<serde_json::Value>,
}
fn exchange(request: Request<'_>, until: Instant) -> Result<Response> {
    let r = active()?;
    let until = until
        .min(r.auth.until)
        .min(Instant::now() + Duration::from_secs(10));
    r.check(until)?;
    let sequence = {
        let mut s = r.state.lock().map_err(|_| refused())?;
        s.sequence = s
            .sequence
            .checked_add(1)
            .filter(|n| *n <= MAX_SAFE)
            .ok_or_else(refused)?;
        s.sequence
    };
    let mut value = serde_json::to_value(request).map_err(|_| refused())?;
    let obj = value.as_object_mut().ok_or_else(refused)?;
    obj.insert("version".into(), 1.into());
    obj.insert("operation".into(), r.operation.clone().into());
    obj.insert("barrierNonce".into(), r.nonce.clone().into());
    obj.insert("sequence".into(), sequence.into());
    let frame = context::runtime_wire(|channel| {
        channel.begin_operation(Deadline(until))?;
        channel.send(&wire::json(&value)?)?;
        channel.receive()
    })?;
    r.check(until)?;
    let reply: Response = serde_json::from_slice(&frame.0).map_err(|_| refused())?;
    let now = context::runtime_ms()?;
    let (bootstrap, _, _, _) = context::runtime_parts()?;
    if reply.version != 1
        || reply.kind != "runtime-owner"
        || reply.operation != r.operation
        || reply.barrier_nonce != r.nonce
        || reply.sequence != sequence
        || reply.lease_expires_at_ms <= now
        || reply.lease_expires_at_ms > now.saturating_add(30000)
        || reply.lease_expires_at_ms > r.auth.expires_at_ms
        || reply.current.binding_json != r.binding
        || reply.current.phase != "consumed"
        || reply.current.epoch != bootstrap.launch.epoch
        || reply.current.closure != "open"
        || reply.current.lineage.reference != bootstrap.lineage.reference
        || reply.current.lineage.database_epoch != bootstrap.lineage.database_epoch
        || !super::hex(&reply.current.global, 64)
        || !super::hex(&reply.current.company, 64)
    {
        return Err(refused());
    }
    Ok(reply)
}
pub(crate) fn runtime_current(until: Instant) -> Result<wire::OwnerCurrent> {
    let reply = exchange(Request::RuntimeCurrent, until)?;
    if reply.store.is_some() || reply.pi.is_some() {
        return Err(refused());
    }
    Ok(reply.current)
}
fn store_reply(reply: Response, expected_phase: Option<&str>) -> Result<serde_json::Value> {
    // Finite domain separation. Actual persisted phase accompanies facts;
    // attempted consumption or an echoed guest Binding cannot supply it.
    if reply.pi.is_some() {
        return Err(refused());
    }
    let store = reply.store.ok_or_else(refused)?;
    let object = store.as_object().ok_or_else(refused)?;
    if object.len() != 2
        || !object.contains_key("facts")
        || !matches!(
            object.get("phase").and_then(serde_json::Value::as_str),
            Some("issued" | "consumed")
        )
        || expected_phase.is_some_and(|phase| {
            object.get("phase").and_then(serde_json::Value::as_str) != Some(phase)
        })
    {
        return Err(refused());
    }
    // This is transport schema only; the root owned-caller service MUST also
    // validate exact launch/lineage/DB/path/purpose/facts and expected phase.
    Ok(store)
}
pub(crate) fn store_startup(until: Instant) -> Result<serde_json::Value> {
    store_reply(exchange(Request::StoreStartup, until)?, Some("issued"))
}
pub(crate) fn store_acquire(
    purpose: StorePurpose,
    attempt: &str,
    until: Instant,
) -> Result<serde_json::Value> {
    if !super::hex(attempt, 32) {
        return Err(refused());
    }
    store_reply(
        exchange(Request::StoreAcquire { purpose, attempt }, until)?,
        Some("issued"),
    )
}
pub(crate) fn store_consume(reference: &str, until: Instant) -> Result<serde_json::Value> {
    if !identifier(reference) {
        return Err(refused());
    }
    // No retry/reconnect here. The per-client grant must burn BEFORE calling.
    store_reply(
        exchange(Request::StoreConsume { reference }, until)?,
        Some("consumed"),
    )
}
pub(crate) fn store_current(reference: &str, until: Instant) -> Result<serde_json::Value> {
    if !identifier(reference) {
        return Err(refused());
    }
    store_reply(exchange(Request::StoreCurrent { reference }, until)?, None)
}
fn pi_reply(
    reply: Response,
    scope: &crate::adapter::pi_guest::owner::OperationScope,
) -> Result<serde_json::Value> {
    // The authenticated operation reply and its actual owner-current wrapper
    // must corroborate the SAME captured scope, not two unrelated fresh reads.
    if reply.store.is_some()
        || reply.current.binding_json != scope.binding_json
        || reply.current.global != scope.global
        || reply.current.company != scope.company
        || reply.current.epoch != scope.epoch
        || reply.current.lineage.reference != scope.lineage
        || reply.current.lineage.database_epoch != scope.database_epoch
    {
        return Err(refused());
    }
    reply.pi.ok_or_else(refused)
}
pub(crate) fn pi_acquire(
    scope: &crate::adapter::pi_guest::owner::OperationScope,
    until: Instant,
) -> Result<serde_json::Value> {
    pi_reply(exchange(Request::PiAcquire { scope }, until)?, scope)
}
pub(crate) fn pi_consume(
    reference: &str,
    scope: &crate::adapter::pi_guest::owner::OperationScope,
    until: Instant,
) -> Result<serde_json::Value> {
    if !super::hex(reference, 32) {
        return Err(refused());
    }
    pi_reply(
        exchange(Request::PiConsume { reference, scope }, until)?,
        scope,
    )
}
pub(crate) fn pi_current(
    reference: &str,
    scope: &crate::adapter::pi_guest::owner::OperationScope,
    until: Instant,
) -> Result<serde_json::Value> {
    if !super::hex(reference, 32) {
        return Err(refused());
    }
    pi_reply(
        exchange(Request::PiCurrent { reference, scope }, until)?,
        scope,
    )
}
pub(super) fn enter(deadline: Deadline) -> Result<()> {
    let frame = context::runtime_wire(|channel| channel.receive())?;
    let release: Release = serde_json::from_slice(&frame.0).map_err(|_| refused())?;
    let (b, root, binding, generation) = context::runtime_parts()?;
    deadline.check()?;
    root.recheck(deadline)?;
    let now = context::runtime_ms()?;
    if now < b.authenticated_at_ms {
        return Err(refused());
    }
    let auth = authenticate(&release, b, binding, generation, now)?;
    RUNTIME
        .set(Runtime {
            root,
            auth,
            operation: b.operation.clone(),
            nonce: b.barrier_nonce.clone(),
            binding: binding.into(),
            state: Mutex::new(State {
                sequence: 0,
                last_wall: now,
            }),
        })
        .map_err(|_| refused())?;
    // First fresh owner readback is required before creating writable mounts,
    // sockets, directories or admitting any protected startup/launch operation.
    runtime_current(Instant::now() + Duration::from_secs(10))?;
    let layout =
        super::layout::Layout::acquire(Deadline(Instant::now() + Duration::from_secs(10)))?;
    // Keep actual root custody and the independently admitted owner carrier
    // alive. No readiness/launch/Store permission is inferred from this pump.
    // The private dispatcher uses the same RuntimeProof and finite exchanges.
    loop {
        let until = Instant::now() + Duration::from_secs(10);
        active()?.check(until)?;
        layout.recheck(Deadline(until))?;
        runtime_current(until)?;
        std::thread::sleep(Duration::from_secs(1));
    }
}
