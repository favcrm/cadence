//! Fixed sealed child transport, NOT authority factories. A root credential on
//! FD3 authenticates only this transport peer; it never elects binary custody.
//! Actual production custody/consume stays with the own-creating root broker.
use super::super::{carrier, files, fixed_arguments, refused, Deadline, Result};
use super::{authenticate_bootstrap, channel, custody, pin, wire};
use crate::daemon::{installer_enrolled, installer_enrollment_wire as receipt};
use std::fs::File;
use std::time::{Duration, Instant};
const FD: i32 = 3;
struct Transport {
    channel: channel::Duplex,
    metadata: wire::ChildContext,
    receipt_keys: Vec<receipt::TrustedKey>,
    grant_keys: Vec<Vec<u8>>,
    deadline: Deadline,
}
impl Transport {
    fn open(role: &str) -> Result<Self> {
        fixed_arguments()?;
        if std::env::vars_os().next().is_some() {
            return Err(refused());
        }
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                FD,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&cred)
            || cred.uid != 0
            || cred.gid != 0
            || cred.pid <= 0
            || cred.pid != unsafe { libc::getppid() }
        {
            return Err(refused());
        }
        let id = if role == "installer" { 21000 } else { 21001 };
        if carrier::ids()? != [id; 6] {
            return Err(refused());
        }
        if unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0
            || unsafe { libc::prctl(libc::PR_GET_SECUREBITS, 0, 0, 0, 0) } != 239
            || unsafe { libc::prctl(libc::PR_GET_KEEPCAPS, 0, 0, 0, 0) } != 0
            || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
            || unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } != 2
        {
            return Err(refused());
        }
        let status = custody::status(std::process::id())?;
        if custody::field(&status, "TracerPid:")? != cred.pid.to_string()
            || custody::field(&status, "Threads:")? != "1"
        {
            return Err(refused());
        }
        for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
            if custody::field(&status, cap)? != "0000000000000000" {
                return Err(refused());
            }
        }
        let mut channel = channel::Duplex::new(FD, FD, Deadline::new())?;
        let frame = channel.receive()?;
        let metadata: wire::ChildContext =
            serde_json::from_slice(&frame.0).map_err(|_| refused())?;
        if metadata.version != 1
            || metadata.kind != "child-context"
            || metadata.role != role
            || metadata.remaining_ms == 0
            || metadata.remaining_ms > 10000
        {
            return Err(refused());
        }
        let deadline = Deadline(Instant::now() + Duration::from_millis(metadata.remaining_ms));
        let bootstrap =
            authenticate_bootstrap(metadata.configure.as_bytes(), super::context::runtime_ms()?)?;
        if wire::binding(&bootstrap, &metadata.installer, &metadata.recipient)?
            != metadata.binding_json
        {
            return Err(refused());
        }
        let (pid, start, pin) = if role == "installer" {
            (
                metadata.installer.pid,
                &metadata.installer.starttime,
                pin(&bootstrap.manifest.artifacts.client)?,
            )
        } else {
            (
                metadata.recipient.pid,
                &metadata.recipient.starttime,
                pin(&bootstrap.manifest.artifacts.supervisor)?,
            )
        };
        if pid != std::process::id()
            || crate::peer::proc_starttime(pid)
                .map(|s| s.to_string())
                .as_ref()
                != Some(start)
        {
            return Err(refused());
        }
        let exe = File::open("/proc/self/exe").map_err(|_| refused())?;
        files::measure(&exe, pin, 0, deadline)?;
        Ok(Self {
            channel,
            metadata,
            receipt_keys: receipt::keys_from_qualified(&bootstrap)?,
            grant_keys: bootstrap.grant_public_keys()?,
            deadline,
        })
    }
    fn receive(&mut self) -> Result<wire::ChildReply> {
        self.deadline.check()?;
        let frame = self.channel.receive()?;
        serde_json::from_slice(&frame.0).map_err(|_| refused())
    }
    fn send(&mut self, frame: &wire::ChildRequest) -> Result<()> {
        self.deadline.check()?;
        self.channel.send(&wire::json(frame)?)
    }
    fn verify(&self, frame: &str) -> Result<Vec<u8>> {
        let keys: Vec<&[u8]> = self.grant_keys.iter().map(|k| k.as_slice()).collect();
        installer_enrolled::verify_constructor_format(
            frame.as_bytes(),
            &self.receipt_keys,
            &keys,
            self.metadata.binding_json.as_bytes(),
            self.deadline.0,
        )
    }
    fn wait_close(&self) -> Result<()> {
        loop {
            self.deadline.wait(FD)?;
            let mut byte = [0u8; 1];
            let n = unsafe { libc::read(FD, byte.as_mut_ptr().cast(), 1) };
            if n == 0 {
                return Ok(());
            }
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
                continue;
            }
            return Err(refused());
        }
    }
}
pub(super) fn client_entry() -> Result<()> {
    if let Some(result) = super::runtime_child::try_entry() {
        return result;
    }
    let mut t = Transport::open("installer")?;
    let frame = match t.receive()? {
        wire::ChildReply::Release { frame } => frame,
        _ => return Err(refused()),
    };
    let expected = t.verify(&frame)?;
    t.send(&wire::ChildRequest::Install {
        frame: frame.clone(),
    })?;
    match t.receive()? {
        wire::ChildReply::Installed { frame: ack } if ack.as_bytes() == expected => {
            t.verify(&frame)?;
            t.send(&wire::ChildRequest::Complete)?;
            t.wait_close()
        }
        _ => Err(refused()),
    }
}
pub(crate) fn recipient_entry() -> Result<()> {
    let mut t = Transport::open("recipient")?;
    let frame = match t.receive()? {
        wire::ChildReply::Install { frame } => frame,
        _ => return Err(refused()),
    };
    let expected = t.verify(&frame)?;
    t.send(&wire::ChildRequest::Ready)?; // format only; never an enrolled/consume success
    match t.receive()? {
        wire::ChildReply::Consumed { frame: ack } if ack.as_bytes() == expected => {
            t.verify(&frame)?;
            t.send(&wire::ChildRequest::Ack { frame: ack })?;
            t.wait_close()
        }
        _ => Err(refused()),
    }
}
