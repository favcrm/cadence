//! CAD-1113: separate private r3 receiver and one-shot waiting-client handoff.
//! Existing UID0 verbs/pins remain unchanged. No signer, remote RPC/ledger,
//! artifact election or production authority constructor. A consumed ACK is
//! NEVER protected launch/retirement. All ambiguities are UNKNOWN, no retry.
#![allow(dead_code)]
use super::{
    installer_client as transport, installer_enrollment_wire as receipt, supervisor_grant as grant,
};
use crate::installer_bundle::{self, ClosedCarrierCustody};
use crate::{Error, Result};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
const MAX_FRAME: usize = 32768;
const BUDGET: Duration = Duration::from_secs(10);
fn unknown() -> Error {
    Error::unknown("enrolled installer UNKNOWN — no retry or launch evidence")
}

// Only observation/consume/connection are present. No enroll/exec/launch call.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EffectCounts {
    connect: u64,
    owner_observe: u64,
    consume: u64,
    enroll: u64,
    exec: u64,
    launch: u64,
}
#[cfg(test)]
thread_local! { static EFFECTS:std::cell::Cell<EffectCounts>=const {std::cell::Cell::new(EffectCounts {connect:0,owner_observe:0,consume:0,enroll:0,exec:0,launch:0})}; }
#[cfg(test)]
fn effect_counts() -> EffectCounts {
    EFFECTS.with(|v| v.get())
}
enum Effect {
    Connect,
    Observe,
    Consume,
}
fn effect(kind: Effect) {
    #[cfg(test)]
    EFFECTS.with(|v| {
        let mut e = v.get();
        match kind {
            Effect::Connect => e.connect += 1,
            Effect::Observe => e.owner_observe += 1,
            Effect::Consume => e.consume += 1,
        }
        v.set(e);
    });
    #[cfg(not(test))]
    let _ = kind;
}

struct SecretFrame(Vec<u8>);
impl Drop for SecretFrame {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}
struct Frame<'a> {
    grant: &'a str,
    receipt: &'a str,
}
impl<'a> Frame<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX_FRAME || bytes.last() != Some(&b'\n') {
            return Err(unknown());
        }
        let text = std::str::from_utf8(&bytes[..bytes.len() - 1]).map_err(|_| unknown())?;
        let fields: Vec<_> = text.split(' ').collect();
        let compact = |s: &str| {
            s.split('.').count() == 3
                && s.split('.').all(|p| {
                    !p.is_empty()
                        && p.bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
                })
        };
        if fields.len() != 3
            || fields[0] != "enrolled-install-r3"
            || !compact(fields[1])
            || !compact(fields[2])
        {
            return Err(unknown());
        }
        Ok(Self {
            grant: fields[1],
            receipt: fields[2],
        })
    }
}
fn acknowledgement(binding: &receipt::InstallerBinding) -> Vec<u8> {
    let c = &binding.challenge;
    format!(
        "ok enrolled-consumed-r3 {} {} {} {}\n",
        c.launch.request.challenge,
        c.launch.request.identity.generation,
        c.recipient.generation,
        binding.barrier_nonce
    )
    .into_bytes()
}
fn classify_ack(binding: &receipt::InstallerBinding, response: &[u8]) -> Result<ConsumptionOnly> {
    if response != acknowledgement(binding) {
        return Err(unknown());
    }
    Ok(ConsumptionOnly)
}
/// No fields granting launch/current/enrollment, no Clone, no exported ctor.
struct ConsumptionOnly;

fn system_ms() -> Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unknown())?
            .as_millis(),
    )
    .map_err(|_| unknown())
}
/// Production selects the kernel wall clock; only private low-level mechanics
/// tests inject a clock dependency. Absolute deadline includes stdin waiting.
struct RequestClock {
    until: Instant,
    last_ms: u64,
    now: fn() -> Result<u64>,
}
impl RequestClock {
    fn new(until: Instant) -> Result<Self> {
        Ok(Self {
            until,
            last_ms: system_ms()?,
            now: system_ms,
        })
    }
    fn check(&mut self) -> Result<u64> {
        transport::Deadline::until(self.until).remaining()?;
        let now = (self.now)()?;
        if now < self.last_ms {
            return Err(unknown());
        }
        self.last_ms = now;
        Ok(now)
    }
    fn valid(&mut self, verified: &VerifiedInstaller) -> Result<()> {
        let now = self.check()?;
        let p = verified.receipt.payload();
        let g = verified.grant.claims();
        if now < p.issued_at_ms
            || now >= p.binding.expires_at_ms
            || now / 1000 < g.nbf
            || now / 1000 > g.exp
        {
            return Err(unknown());
        }
        Ok(())
    }
}
struct VerifiedInstaller {
    receipt: receipt::VerifiedReceipt,
    grant: grant::VerifiedEnvelope,
}
impl VerifiedInstaller {
    fn verify(
        frame: &Frame<'_>,
        keys: &[receipt::TrustedKey],
        grant_keys: &[&[u8]],
        clock: &mut RequestClock,
    ) -> Result<Self> {
        let now = clock.check()?;
        let left = transport::Deadline::until(clock.until).remaining()?;
        let elapsed = BUDGET.saturating_sub(left).as_millis() as u64;
        let receipt = receipt::verify_receipt_format(frame.receipt, keys, now, elapsed)
            .map_err(|_| unknown())?;
        let grant = grant::verify_enrolled_format(
            frame.grant,
            grant_keys,
            now / 1000,
            receipt.binding_json(),
        )
        .map_err(|_| unknown())?;
        let out = Self { receipt, grant };
        clock.valid(&out)?;
        Ok(out)
    }
    fn binding(&self) -> &receipt::InstallerBinding {
        &self.receipt.payload().binding
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Prepared,
    Consumed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closure {
    Open,
    Closed,
    Unknown,
}
/// Opaque authenticated OWNER revisions (not combined record phase/history).
/// Adapter translates actual ExecutorAdmission+CompanyControl snapshots; no
/// guessed RPC schema. Revision must bind current launch/epoch/closure owner.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnerStamp {
    global: Vec<u8>,
    company: Vec<u8>,
    epoch: u64,
    lineage: grant::Lineage,
    closure: Closure,
}
struct OwnerSnapshot {
    binding_json: Vec<u8>,
    phase: Phase,
    stamp: OwnerStamp,
}
impl OwnerSnapshot {
    fn validate(&self, verified: &VerifiedInstaller, expected: Phase) -> Result<()> {
        let sc = &verified.grant.claims().challenge;
        if self.binding_json != verified.receipt.binding_json()
            || self.phase != expected
            || self.stamp.closure != Closure::Open
            || self.stamp.epoch != sc.launch.epoch
            || self.stamp.lineage != sc.lineage
            || self.stamp.global.is_empty()
            || self.stamp.global.len() > 256
            || self.stamp.company.is_empty()
            || self.stamp.company.len() > 256
        {
            return Err(unknown());
        }
        Ok(())
    }
}
/// Phase-specific authenticated port, existing external owner ONLY. No enroll,
/// retire, generic phase selector, local tombstone or caller-literal adapter.
/// Each await checks same owner/epoch/closure/lineage/full record before+after;
/// adapters MUST obey the absolute deadline. No production implementation yet.
trait CombinedConsume {
    fn observe_prepared(
        &self,
        verified: &VerifiedInstaller,
        until: Instant,
    ) -> Result<OwnerSnapshot>;
    fn consume_once(
        &self,
        prepared: &PreparedInstaller<'_>,
        until: Instant,
    ) -> Result<grant::ConsumeOutcome>;
    fn observe_consumed_current(
        &self,
        prepared: &PreparedInstaller<'_>,
        until: Instant,
    ) -> Result<OwnerSnapshot>;
}
fn production_owner_port() -> Result<Box<dyn CombinedConsume>> {
    Err(unknown())
}
fn production_recipient_enrollment() -> Result<transport::Enrollment> {
    Err(unknown())
}
struct ReleaseAuthority {
    custody: ClosedCarrierCustody,
    receipt_keys: &'static [receipt::TrustedKey],
    grant_keys: &'static [&'static [u8]],
    supervisor: transport::Enrollment,
    owner: Box<dyn CombinedConsume>,
}
fn production_release_authority() -> Result<ReleaseAuthority> {
    // Separate immutable carrier seal/no-exec custody BEFORE proof or effects.
    // Receipt signatures and diagnostics can never create this private input.
    let custody = installer_bundle::production_closed_custody()?;
    Ok(ReleaseAuthority {
        custody,
        receipt_keys: receipt::production_trust_set()?,
        grant_keys: grant::production_grant_keyring()?,
        supervisor: production_recipient_enrollment()?,
        owner: production_owner_port()?,
    })
}
fn recipient_matches(
    verified: &VerifiedInstaller,
    supervisor: &transport::Enrollment,
) -> Result<()> {
    let r = &verified.binding().challenge.recipient;
    if r.pid != supervisor.pid
        || r.starttime != supervisor.starttime.to_string()
        || r.generation != supervisor.generation
        || supervisor.digest == [0; 32]
    {
        return Err(unknown());
    }
    Ok(())
}
struct PreparedInstaller<'a> {
    verified: &'a VerifiedInstaller,
    stamp: OwnerStamp,
}
fn prepared<'a>(
    verified: &'a VerifiedInstaller,
    owner: &dyn CombinedConsume,
    clock: &mut RequestClock,
) -> Result<PreparedInstaller<'a>> {
    clock.valid(verified)?;
    effect(Effect::Observe);
    let view = owner
        .observe_prepared(verified, clock.until)
        .map_err(|_| unknown())?;
    clock.valid(verified)?;
    view.validate(verified, Phase::Prepared)?;
    Ok(PreparedInstaller {
        verified,
        stamp: view.stamp,
    })
}
fn recheck_prepared(
    prepared: &PreparedInstaller<'_>,
    owner: &dyn CombinedConsume,
    clock: &mut RequestClock,
) -> Result<()> {
    let again = self::prepared(prepared.verified, owner, clock)?;
    if again.stamp != prepared.stamp {
        return Err(unknown());
    }
    Ok(())
}
/// Dependency-explicit PRIVATE mechanics, never callable by JSON/CLI/RPC.
/// Production check closure revalidates held kernel peer AND private custody;
/// tests may model that step but never open/override any production factory.
fn consume_prepared(
    prepared: PreparedInstaller<'_>,
    owner: &dyn CombinedConsume,
    clock: &mut RequestClock,
    mut peer_recheck: impl FnMut() -> Result<()>,
) -> Result<ConsumptionOnly> {
    recheck_prepared(&prepared, owner, clock)?;
    peer_recheck()?;
    clock.valid(prepared.verified)?;
    effect(Effect::Consume);
    if owner
        .consume_once(&prepared, clock.until)
        .map_err(|_| unknown())?
        != grant::ConsumeOutcome::Consumed
    {
        return Err(unknown());
    }
    clock.valid(prepared.verified)?;
    peer_recheck()?;
    effect(Effect::Observe);
    let current = owner
        .observe_consumed_current(&prepared, clock.until)
        .map_err(|_| unknown())?;
    clock.valid(prepared.verified)?;
    current.validate(prepared.verified, Phase::Consumed)?;
    if current.stamp != prepared.stamp {
        return Err(unknown());
    }
    peer_recheck()?;
    clock.valid(prepared.verified)?;
    Ok(ConsumptionOnly) // no spawn, launch or retirement follows
}

struct EnrolledPeer {
    pid: u32,
    observation: installer_bundle::CustodyObservation,
}
fn socket_peer(stream: &UnixStream) -> Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&cred)
        || cred.uid != 21000
        || cred.gid != 21000
        || cred.pid <= 0
    {
        return Err(unknown());
    }
    Ok(cred.pid as u32)
}
impl EnrolledPeer {
    fn admit(
        stream: &UnixStream,
        verified: &VerifiedInstaller,
        custody: &ClosedCarrierCustody,
        until: Instant,
    ) -> Result<Self> {
        let pid = socket_peer(stream)?;
        if pid != verified.binding().installer.pid {
            return Err(unknown());
        }
        let observation = custody
            .observe_record(&verified.binding().installer, until)
            .map_err(|_| unknown())?;
        let out = Self { pid, observation };
        out.recheck(stream, custody, until)?;
        Ok(out)
    }
    fn recheck(
        &self,
        stream: &UnixStream,
        custody: &ClosedCarrierCustody,
        until: Instant,
    ) -> Result<()> {
        if socket_peer(stream)? != self.pid {
            return Err(unknown());
        }
        custody
            .recheck(&self.observation, until)
            .map_err(|_| unknown())
    }
}

pub(super) struct EnrolledReceiver {
    authority: ReleaseAuthority,
}
pub(super) fn production_enrolled_receiver() -> Result<EnrolledReceiver> {
    Ok(EnrolledReceiver {
        authority: production_release_authority()?,
    })
}
pub(super) fn handle_enrolled_stream(stream: &UnixStream) -> Result<()> {
    production_enrolled_receiver()?.serve(stream)
}
impl EnrolledReceiver {
    pub(super) fn serve(self, stream: &UnixStream) -> Result<()> {
        self.serve_until(stream, Instant::now() + BUDGET)
    }
    pub(super) fn serve_until(self, stream: &UnixStream, until: Instant) -> Result<()> {
        let until = until.min(Instant::now() + BUDGET);
        let mut clock = RequestClock::new(until)?;
        self.authority.custody.require_local_principal(until)?;
        transport::measured_process(
            std::process::id(),
            &self.authority.supervisor,
            &self.authority.supervisor.generation,
        )?;
        let bytes = SecretFrame(
            transport::read_response(stream, transport::Deadline::until(until))
                .map_err(|_| unknown())?,
        );
        let frame = Frame::parse(&bytes.0)?;
        let verified = VerifiedInstaller::verify(
            &frame,
            self.authority.receipt_keys,
            self.authority.grant_keys,
            &mut clock,
        )?;
        recipient_matches(&verified, &self.authority.supervisor)?;
        // The receiver MUST itself be the independently enrolled recipient.
        if self.authority.supervisor.pid != std::process::id()
            || crate::peer::proc_starttime(std::process::id())
                != Some(self.authority.supervisor.starttime)
        {
            return Err(unknown());
        }
        let peer = EnrolledPeer::admit(stream, &verified, &self.authority.custody, until)?;
        let prepared = prepared(&verified, self.authority.owner.as_ref(), &mut clock)?;
        let _consumed =
            consume_prepared(prepared, self.authority.owner.as_ref(), &mut clock, || {
                self.authority.custody.require_local_principal(until)?;
                transport::measured_process(
                    std::process::id(),
                    &self.authority.supervisor,
                    &self.authority.supervisor.generation,
                )?;
                peer.recheck(stream, &self.authority.custody, until)
            })?;
        peer.recheck(stream, &self.authority.custody, until)?;
        clock.valid(&verified)?;
        transport::write_frame(
            stream,
            &acknowledgement(verified.binding()),
            transport::Deadline::until(until),
        )
        .map_err(|_| unknown())?;
        stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|_| unknown())?;
        clock.valid(&verified)?;
        Ok(())
    }
}

/// One-shot release capability, bound to independently qualified construction,
/// actual sealed self, verified signed full binding and authenticated PREPARED.
/// No Clone/export/test constructor/caller proof can create a release build one.
struct ReleasedInstaller<'a> {
    verified: VerifiedInstaller,
    authority: ReleaseAuthority,
    observed: installer_bundle::CustodyObservation,
    frame: &'a [u8],
    stamp: OwnerStamp,
}
impl<'a> ReleasedInstaller<'a> {
    fn prepare(
        frame: &'a [u8],
        authority: ReleaseAuthority,
        clock: &mut RequestClock,
    ) -> Result<Self> {
        let parsed = Frame::parse(frame)?;
        let verified = VerifiedInstaller::verify(
            &parsed,
            authority.receipt_keys,
            authority.grant_keys,
            clock,
        )?;
        recipient_matches(&verified, &authority.supervisor)?;
        if verified.binding().installer.pid != std::process::id() {
            return Err(unknown());
        }
        let observed = authority
            .custody
            .observe_record(&verified.binding().installer, clock.until)
            .map_err(|_| unknown())?;
        let prepared = prepared(&verified, authority.owner.as_ref(), clock)?;
        let stamp = prepared.stamp;
        authority.custody.recheck(&observed, clock.until)?;
        clock.valid(&verified)?;
        Ok(Self {
            verified,
            authority,
            observed,
            frame,
            stamp,
        })
    }
    fn send_once(self, clock: &mut RequestClock) -> Result<ConsumptionOnly> {
        let pending = PreparedInstaller {
            verified: &self.verified,
            stamp: self.stamp,
        };
        recheck_prepared(&pending, self.authority.owner.as_ref(), clock)?;
        self.authority
            .custody
            .recheck(&self.observed, clock.until)?;
        clock.valid(&self.verified)?;
        let topology = transport::Topology::capture(transport::POLICY)?;
        let deadline = transport::Deadline::until(clock.until);
        effect(Effect::Connect);
        let stream = topology.connect(deadline)?;
        topology.recheck(transport::POLICY)?;
        let generation = &self.verified.binding().challenge.recipient.generation;
        transport::admit_supervisor(&stream, &self.authority.supervisor, generation)?;
        self.authority
            .custody
            .recheck(&self.observed, clock.until)?;
        recheck_prepared(&pending, self.authority.owner.as_ref(), clock)?;
        clock.valid(&self.verified)?;
        // BOTH existing envelopes cross the private socket. Never grant only.
        transport::write_frame(&stream, self.frame, deadline)?;
        stream
            .shutdown(std::net::Shutdown::Write)
            .map_err(|_| unknown())?;
        let response = transport::read_response(&stream, deadline)?;
        topology.recheck(transport::POLICY)?;
        transport::admit_supervisor(&stream, &self.authority.supervisor, generation)?;
        self.authority
            .custody
            .recheck(&self.observed, clock.until)?;
        clock.valid(&self.verified)?;
        effect(Effect::Observe);
        let current = self
            .authority
            .owner
            .observe_consumed_current(&pending, clock.until)
            .map_err(|_| unknown())?;
        clock.valid(&self.verified)?;
        current.validate(&self.verified, Phase::Consumed)?;
        if current.stamp != pending.stamp {
            return Err(unknown());
        }
        classify_ack(self.verified.binding(), &response)
    }
}
/// Actual private production release path, usable by independent guard without
/// argv. Missing closed custody refuses before parser/owner/connect/consume.
pub(crate) fn release_host_frame(frame: &[u8]) -> Result<()> {
    release_host_frame_until(frame, Instant::now() + BUDGET)
}
pub(crate) fn release_host_frame_until(frame: &[u8], until: Instant) -> Result<()> {
    let until = until.min(Instant::now() + BUDGET); // never extend the stdin deadline
    let mut clock = RequestClock::new(until)?;
    let authority = production_release_authority()?;
    let released = ReleasedInstaller::prepare(frame, authority, &mut clock)?;
    let _consumption_ack_only = released.send_once(&mut clock)?;
    Ok(())
}

#[cfg(test)]
mod tests;
