//! Fixed ROOT-owned helper creation. No PID adoption/command/environment/FD
//! number API; selected argv is derived only from an opaque launch permit.
use super::super::{files, refused, Deadline, Result};
use super::helper_process::Process;
use super::{custody, lifecycle};
use crate::adapter::pi_guest::owner::LaunchPermit;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};
#[derive(Clone, Copy)]
pub(crate) enum HelperPhase {
    Privileged,
    Sealed,
}
pub(crate) struct HelperStdio {
    pub stdin: File,
    pub stdout: File,
    pub stderr: File,
}
/// Non-Clone actual own-created child, selected executable, pidfd and birth.
/// SO_PEERCRED is checked against THIS retained handle, not supplied metadata.
// Non-Clone kernel retirement evidence, never decoded from wire or ACK/PID.
struct RetiredFamily {
    _pidfd: File,
}
pub(crate) struct OwnedHelper {
    child: Process,
    pidfd: File,
    birth: u64,
    artifact: files::HeldArtifact,
    guest: u32,
    guest_gid: u32,
    parent: u32,
    launch_gid: u32,
    sealed_groups: Vec<u32>,
    namespaces: Vec<File>,
    node: std::sync::OnceLock<File>,
    retired: std::sync::OnceLock<RetiredFamily>,
}
impl OwnedHelper {
    pub(crate) fn spawn(permit: &LaunchPermit, until: Instant) -> Result<(Self, HelperStdio)> {
        let deadline = Deadline(until.min(Instant::now() + Duration::from_secs(30)));
        lifecycle::runtime_proof(until)?.recheck(until)?;
        permit.recheck()?;
        let selected = permit.describe();
        selected
            .validate(&selected.selection)
            .map_err(|_| refused())?;
        let artifact = files::HeldArtifact::open(
            files::Artifact::Helper,
            selected.image.helper_sha256,
            deadline,
        )?;
        let routing = crate::protected_pi_profile::Routing::for_agent(
            selected.selection.role == crate::protected_pi_profile::authority::Role::Master,
            &selected.selection.model,
        )
        .map_err(|_| refused())?;
        let mut args = vec![
            "exec".to_owned(),
            "--profile".into(),
            "pi-guest".into(),
            format!("--alias-sha256={}", selected.selection.alias_sha256),
            format!("--generation={}", selected.selection.generation),
            "--".into(),
        ];
        args.extend(routing.tokens());
        let strings: Vec<std::ffi::CString> = std::iter::once("cadence-agent-exec".to_owned())
            .chain(args)
            .map(|s| std::ffi::CString::new(s).map_err(|_| refused()))
            .collect::<Result<_>>()?;
        let parent = std::process::id();
        let launch_gid = crate::adapter::pi_guest::topology::resolve_gid_pub("cadence-launch")?;
        if launch_gid == 0 {
            return Err(refused());
        }
        let mut sealed_groups = vec![selected.guest_gid, selected.shared_gid];
        sealed_groups.sort_unstable();
        sealed_groups.dedup();
        // Own-created namespace PID1 and atomic pidfd, held at selected helper
        // exec before ANY helper instruction. No fork/pgrp/PID-adoption fallback.
        let mut child = Process::spawn(&artifact, &strings, launch_gid, deadline)?;
        let pid = child.id();
        let setup = (|| {
            let birth = crate::peer::proc_starttime(pid).ok_or_else(refused)?;
            let pidfd = child.pidfd()?;
            artifact.live_correspondence(pid, deadline)?;
            lifecycle::runtime_proof(until)?.recheck(until)?;
            permit.recheck()?;
            let namespaces = ["user", "pid", "mnt"]
                .iter()
                .map(|name| File::open(format!("/proc/{pid}/ns/{name}")).map_err(|_| refused()))
                .collect::<Result<Vec<_>>>()?;
            let stdio = HelperStdio {
                stdin: child.stdin.take().ok_or_else(refused)?,
                stdout: child.stdout.take().ok_or_else(refused)?,
                stderr: child.stderr.take().ok_or_else(refused)?,
            };
            Ok((pidfd, birth, stdio, namespaces))
        })();
        match setup {
            Ok((pidfd, birth, stdio, namespaces)) => {
                let owned = Self {
                    child,
                    pidfd,
                    birth,
                    artifact,
                    guest: selected.guest,
                    guest_gid: selected.guest_gid,
                    parent,
                    launch_gid,
                    sealed_groups,
                    namespaces,
                    node: std::sync::OnceLock::new(),
                    retired: std::sync::OnceLock::new(),
                };
                owned.namespaces_current()?;
                owned.child.release()?;
                Ok((owned, stdio))
            }
            Err(e) => {
                child.kill();
                let until = Instant::now() + Duration::from_secs(2);
                while Instant::now() < until {
                    let mut status = 0;
                    let rc = unsafe {
                        libc::waitpid(child.id() as i32, &mut status, libc::WNOHANG | libc::__WALL)
                    };
                    if rc < 0 || (rc > 0 && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status)))
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e)
            }
        }
    }
    /// Continue ONLY the authenticated selected Node exec after durable consume.
    /// Unexpected trace stops/exit refuse; no caller exec/address is accepted.
    pub(crate) fn release_node(&self, permit: &LaunchPermit, until: Instant) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let deadline = Deadline(until);
        permit.recheck()?;
        loop {
            deadline.check()?;
            let mut status = 0;
            let rc = unsafe {
                libc::waitpid(
                    self.child.id() as i32,
                    &mut status,
                    libc::WNOHANG | libc::__WALL,
                )
            };
            if rc < 0 {
                return Err(refused());
            }
            if rc == 0 {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            if !libc::WIFSTOPPED(status)
                || libc::WSTOPSIG(status) != libc::SIGTRAP
                || (status >> 16) != libc::PTRACE_EVENT_EXEC
            {
                return Err(refused());
            }
            let live =
                File::open(format!("/proc/{}/exe", self.child.id())).map_err(|_| refused())?;
            let selected =
                File::open(crate::protected_pi_profile::NODE_PATH).map_err(|_| refused())?;
            let m = live.metadata().map_err(|_| refused())?;
            let s = selected.metadata().map_err(|_| refused())?;
            if (m.dev(), m.ino(), m.size()) != (s.dev(), s.ino(), s.size())
                || m.uid() != 0
                || m.gid() != 0
                || m.mode() & 0o7777 != 0o755
                || m.nlink() != 1
                || m.size() > 128 * 1024 * 1024
                || crate::adapter::pi_guest::execfd::sha256_fd(&live, m.size())
                    .map_err(|_| refused())?
                    != permit.describe().image.node_sha256
            {
                return Err(refused());
            }
            let text = custody::status(self.child.id())?;
            if custody::field(&text, "NoNewPrivs:")? != "1" {
                return Err(refused());
            }
            for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
                if custody::field(&text, cap)? != "0000000000000000" {
                    return Err(refused());
                }
            }
            permit.recheck()?;
            lifecycle::runtime_proof(until)?.recheck(until)?;
            super::dispatcher::launch_live(until)?;
            self.namespaces_current()?;
            self.node.set(live).map_err(|_| refused())?;
            if unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    self.child.id() as i32,
                    std::ptr::null_mut::<libc::c_void>(),
                    std::ptr::null_mut::<libc::c_void>(),
                )
            } != 0
            {
                return Err(refused());
            }
            return deadline.check();
        }
    }
    pub(crate) fn require_peer(
        &self,
        stream: &UnixStream,
        phase: HelperPhase,
        until: Instant,
    ) -> Result<()> {
        let deadline = Deadline(until);
        lifecycle::runtime_proof(until)?.recheck(until)?;
        self.namespaces_current()?;
        let mut ready = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut ready, 1, 0) } != 0
            || crate::peer::proc_starttime(self.child.id()) != Some(self.birth)
        {
            return Err(refused());
        }
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of::<libc::ucred>()
            || cred.pid != self.child.id() as i32
            || cred.uid != 0
            || cred.gid != 0
        {
            return Err(refused());
        }
        self.artifact
            .live_correspondence(self.child.id(), deadline)?;
        let text = custody::status(self.child.id())?;
        let ids = |key: &str| -> Result<Vec<u32>> {
            custody::field(&text, key)?
                .split_ascii_whitespace()
                .map(|s| s.parse().map_err(|_| refused()))
                .collect()
        };
        if custody::field(&text, "TracerPid:")?
            .parse::<u32>()
            .map_err(|_| refused())?
            != self.parent
        {
            return Err(refused());
        }
        match phase {
            HelperPhase::Privileged => {
                if ids("Uid:")? != [21000, 0, 0, 0]
                    || ids("Gid:")? != [21000, 0, 0, 0]
                    || ids("Groups:")? != [self.launch_gid]
                {
                    return Err(refused());
                }
            }
            HelperPhase::Sealed => {
                if ids("Uid:")? != [self.guest; 4]
                    || ids("Gid:")? != [self.guest_gid; 4]
                    || custody::field(&text, "NoNewPrivs:")? != "1"
                    || ids("Groups:")? != self.sealed_groups
                {
                    return Err(refused());
                }
                for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
                    if custody::field(&text, cap)? != "0000000000000000" {
                        return Err(refused());
                    }
                }
            }
        }
        deadline.check()
    }
    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }
    pub(crate) fn interrupt(&self, until: Instant) -> Result<()> {
        self.signal(libc::SIGINT, until)
    }
    fn namespaces_current(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        for (name, held) in ["user", "pid", "mnt"].iter().zip(&self.namespaces) {
            let a = held.metadata().map_err(|_| refused())?;
            let b = File::open(format!("/proc/{}/ns/{name}", self.child.id()))
                .map_err(|_| refused())?
                .metadata()
                .map_err(|_| refused())?;
            let parent = File::open(format!("/proc/self/ns/{name}"))
                .map_err(|_| refused())?
                .metadata()
                .map_err(|_| refused())?;
            if (a.dev(), a.ino()) != (b.dev(), b.ino())
                || (*name == "pid" && (a.dev(), a.ino()) == (parent.dev(), parent.ino()))
                || (*name != "pid" && (a.dev(), a.ino()) != (parent.dev(), parent.ino()))
            {
                return Err(refused());
            }
        }
        let status = custody::status(self.child.id())?;
        let ids = custody::field(&status, "NSpid:")?
            .split_ascii_whitespace()
            .map(|s| s.parse::<u32>().map_err(|_| refused()))
            .collect::<Result<Vec<_>>>()?;
        let parent = custody::status(std::process::id())?;
        let depth = custody::field(&parent, "NSpid:")?
            .split_ascii_whitespace()
            .count();
        if ids.len() != depth + 1 || ids.first() != Some(&self.child.id()) || ids.last() != Some(&1)
        {
            return Err(refused());
        }
        Ok(())
    }
    fn node_current(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        self.namespaces_current()?;
        let a = self
            .node
            .get()
            .ok_or_else(refused)?
            .metadata()
            .map_err(|_| refused())?;
        let b = File::open(format!("/proc/{}/exe", self.child.id()))
            .map_err(|_| refused())?
            .metadata()
            .map_err(|_| refused())?;
        if (a.dev(), a.ino(), a.size(), a.ctime(), a.ctime_nsec())
            != (b.dev(), b.ino(), b.size(), b.ctime(), b.ctime_nsec())
        {
            return Err(refused());
        }
        let text = custody::status(self.child.id())?;
        let ids = |key: &str| -> Result<Vec<u32>> {
            custody::field(&text, key)?
                .split_ascii_whitespace()
                .map(|s| s.parse::<u32>().map_err(|_| refused()))
                .collect()
        };
        if ids("Uid:")? != [self.guest; 4]
            || ids("Gid:")? != [self.guest_gid; 4]
            || ids("Groups:")? != self.sealed_groups
        {
            return Err(refused());
        }
        if custody::field(&text, "TracerPid:")? != self.parent.to_string()
            || custody::field(&text, "NoNewPrivs:")? != "1"
        {
            return Err(refused());
        }
        for cap in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
            if custody::field(&text, cap)? != "0000000000000000" {
                return Err(refused());
            }
        }
        Ok(())
    }
    fn signal(&self, signal: i32, until: Instant) -> Result<()> {
        lifecycle::runtime_proof(until)?.recheck(until)?;
        self.node_current()?;
        if crate::peer::proc_starttime(self.child.id()) != Some(self.birth) {
            return Err(refused());
        }
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        } != 0
        {
            return Err(refused());
        }
        lifecycle::runtime_proof(until)?.recheck(until)
    }
    pub(crate) fn exited(&self, until: Instant) -> Result<bool> {
        lifecycle::runtime_proof(until)?.recheck(until)?;
        let mut ready = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut ready, 1, 0) };
        if n < 0 {
            return Err(refused());
        }
        if n == 0 {
            if crate::peer::proc_starttime(self.child.id()) != Some(self.birth) {
                return Err(refused());
            }
            self.node_current()?;
        }
        let mut status = 0;
        let rc = unsafe {
            libc::waitpid(
                self.child.id() as i32,
                &mut status,
                libc::WNOHANG | libc::__WALL,
            )
        };
        if rc > 0 {
            if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                lifecycle::runtime_proof(until)?.recheck(until)?;
                return Ok(true); // actual retained-child kernel exit only
            }
            // SEIZE keeps normal POSIX signal-delivery stops, including tool
            // SIGCHLD and owned SIGINT. Never suppress them or leave Node stuck.
            // A later exec/event/trap/group-stop is NOT a release permission.
            if !libc::WIFSTOPPED(status)
                || status >> 16 != 0
                || matches!(libc::WSTOPSIG(status), libc::SIGTRAP | libc::SIGSTOP)
            {
                return Err(refused());
            }
            self.node_current()?;
            lifecycle::runtime_proof(until)?.recheck(until)?;
            if unsafe {
                libc::ptrace(
                    libc::PTRACE_CONT,
                    self.child.id() as i32,
                    0usize,
                    libc::WSTOPSIG(status) as usize,
                )
            } != 0
            {
                return Err(refused());
            }
        }
        if rc < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD) {
            return Err(refused());
        }
        lifecycle::runtime_proof(until)?.recheck(until)?;
        Ok(n > 0 && ready.revents & libc::POLLIN != 0)
    }
    pub(crate) fn retire(&self, until: Instant) -> Result<()> {
        let until = until.min(Instant::now() + Duration::from_secs(10));
        // This ACTUAL owned task is namespace init. Its kernel exit/reap is
        // ordered after namespace descendant kill+reap, not a guessed PGID/list.
        lifecycle::runtime_proof(until)?.recheck(until)?;
        if !self.exited(until)? {
            self.signal(libc::SIGKILL, until)?;
        }
        while !self.exited(until)? {
            Deadline(until).check()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        lifecycle::runtime_proof(until)?.recheck(until)?;
        if self.retired.get().is_none() {
            self.retired
                .set(RetiredFamily {
                    _pidfd: self.pidfd.try_clone().map_err(|_| refused())?,
                })
                .map_err(|_| refused())?;
        }
        Ok(()) // physical result ONLY; not external durable retirement
    }
    pub(crate) fn retirement_verified(&self, until: Instant) -> Result<bool> {
        Ok(self.retired.get().is_some() && self.exited(until)?)
    }
}
impl Drop for OwnedHelper {
    fn drop(&mut self) {
        unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                libc::SIGKILL,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            );
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until {
            let mut status = 0;
            let n = unsafe {
                libc::waitpid(
                    self.child.id() as i32,
                    &mut status,
                    libc::WNOHANG | libc::__WALL,
                )
            };
            if n < 0 || (n > 0 && (libc::WIFEXITED(status) || libc::WIFSIGNALED(status))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
