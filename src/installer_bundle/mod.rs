//! CAD-1113 fixed Linux x86_64 installer mechanics, not runtime qualification.
//! Only fixed no-argument executable entries escape this private module.
//! Proposed paths below are UNELECTED; no image pins/host custody exist yet.
#![allow(dead_code)]
mod carrier;
mod files;
mod observer;
// Reuse the reviewed helper's actual seal, not its setuid policy/command parser.
#[path = "../bin/cadence-agent-exec/capabilities.rs"]
mod seal;

use crate::{Error, Result};
use std::time::{Duration, Instant};
const CLIENT: &str = "/opt/protected/bin/cadence-installer-client";
const CARRIER: &str = "/opt/protected/bin/cadence-grant-install";
const OBSERVER: &str = "/opt/protected/bin/cadence-installer-observer";
const IDS: u32 = 21000;
const BUDGET: Duration = Duration::from_secs(10);

fn refused() -> Error { Error::unknown("fixed installer UNKNOWN — no authority or retry") }
fn fixed_arguments() -> Result<()> {
    if std::env::args_os().count() != 1 { return Err(refused()); }
    Ok(())
}
#[derive(Clone, Copy)]
struct Deadline(Instant);
impl Deadline {
    fn new() -> Self { Self(Instant::now() + BUDGET) }
    fn check(self) -> Result<()> {
        if Instant::now() >= self.0 { return Err(refused()); }
        Ok(())
    }
    fn wait(self, fd: std::os::fd::RawFd) -> Result<()> {
        loop {
            self.check()?;
            let ms = self.0.saturating_duration_since(Instant::now()).as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
            let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let rc = unsafe { libc::poll(&mut p, 1, ms) };
            self.check()?;
            if rc > 0 && p.revents & libc::POLLNVAL == 0 { return Ok(()); }
            if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted { continue; }
            return Err(refused());
        }
    }
}

/// Only an independently qualified immutable bootstrap/image owner may produce
/// this input. No CLI/env/JSON/test-seam constructor in a shipped build.
struct QualifiedImage {
    client: [u8; 32],
    carrier: [u8; 32],
    observer: [u8; 32],
    namespaces: observer::Namespaces,
}
fn production_image() -> Result<QualifiedImage> { Err(refused()) }

/// Separate independently qualified immutable seal/no-alternate-exec/process
/// custody. Not procfs observation, caller assertion, stdout or signature.
/// Fields are private, no JSON/CLI/env/public/test-seam production constructor.
pub(crate) struct ClosedCarrierCustody { image:QualifiedImage, pid:u32, starttime:u64 }
pub(crate) fn production_closed_custody()->Result<ClosedCarrierCustody> {
    Err(refused()) // exact host-held process construction evidence unavailable
}
pub(crate) struct CustodyObservation { facts:observer::Diagnostic }
impl ClosedCarrierCustody {
    pub(crate) fn observe_record(&self,record:&crate::daemon::InstallerProcessRecord,until:Instant)->Result<CustodyObservation> {
        let hex=|b:&[u8;32]|b.iter().map(|v|format!("{v:02x}")).collect::<String>();
        if record.pid!=self.pid || record.starttime!=self.starttime.to_string()
            || record.uid!=u64::from(IDS) || record.gid!=u64::from(IDS)
            || record.client_digest!=hex(&self.image.client) || record.carrier_digest!=hex(&self.image.carrier)
            || record.observer_digest!=hex(&self.image.observer) {return Err(refused());}
        let deadline=Deadline(until);
        // Current image artifacts and target measurements corroborate this
        // private provenance; they NEVER create it or fill remote prctl facts.
        for (kind,pin) in [(files::Artifact::Carrier,self.image.carrier),(files::Artifact::Observer,self.image.observer)] {
            let held=files::HeldArtifact::open(kind,pin,deadline)?;held.recheck(deadline)?;
        }
        let facts=observer::observe(self.pid,&self.image,deadline)?;
        if facts.starttime()!=self.starttime {return Err(refused());}
        Ok(CustodyObservation {facts})
    }
    pub(crate) fn recheck(&self,seen:&CustodyObservation,until:Instant)->Result<()> {
        if seen.facts.starttime()!=self.starttime {return Err(refused());}
        seen.facts.recheck(&self.image,Deadline(until))
    }
}

/// Construction is separate from externally enrolled RELEASE authority. It
/// authorizes only one withheld host stdin read, never connect/consume/launch.
struct WaitingConstruction { image: QualifiedImage }
fn production_construction() -> Result<WaitingConstruction> {
    let _image = production_image()?;
    Err(refused()) // no independently qualified protected bootstrap/custody
}

pub(crate) fn carrier_entry() -> Result<()> { carrier::entry() }
pub(crate) fn observer_entry() -> Result<()> { observer::entry() }
pub(crate) fn client_entry() -> Result<()> {
    fixed_arguments()?;
    let deadline = Deadline::new();
    let construction = production_construction()?; // before reading host stdin
    let stdin = std::io::stdin();
    use std::os::fd::AsRawFd;
    // Construction permits only this withheld read. No further exec/fork,
    // connection or consume before an independently verified owner release.
    let self_facts = observer::observe(std::process::id(), &construction.image, deadline)?;
    self_facts.require_construction_measurement()?;
    let frame = waiting_frame(stdin.as_raw_fd(), deadline)?;
    crate::daemon::installer_enrolled::release_host_frame_until(&frame.0,deadline.0)
    // Ok is consumption-only transport evidence, NEVER launch or retirement.
}

struct HostFrame(Vec<u8>);
impl Drop for HostFrame { fn drop(&mut self) { self.0.fill(0); } }
#[derive(Clone, Copy)]
enum FrameKind { Release, Observation }
fn waiting_frame(fd: std::os::fd::RawFd, deadline: Deadline) -> Result<HostFrame> {
    read_host_frame(fd,deadline,FrameKind::Release)
}
fn read_host_frame(fd:std::os::fd::RawFd,deadline:Deadline,kind:FrameKind)->Result<HostFrame> {
    let max=match kind {FrameKind::Release=>32768,FrameKind::Observation=>256};
    let mut frame = HostFrame(Vec::with_capacity(1024));
    let mut chunk = [0u8; 1024];
    loop {
        deadline.wait(fd)?;
        let want = chunk.len().min(max + 1 - frame.0.len());
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), want) };
        deadline.check()?;
        if n == 0 { break; }
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted { continue; }
            return Err(refused());
        }
        frame.0.extend_from_slice(&chunk[..n as usize]);
        chunk.fill(0);
        if frame.0.len() > max { return Err(refused()); }
    }
    if frame.0.last() != Some(&b'\n') || frame.0[..frame.0.len()-1].iter().any(|b| !b.is_ascii() || b"\n\r\0\t".contains(b)) { return Err(refused()); }
    Ok(frame) // framing evidence ONLY, cannot be a release capability
}

#[cfg(test)]
mod tests;
