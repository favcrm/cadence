//! Actual constructor-owned mount/principal custody. A signed hash or an
//! operator-supplied verified boolean does not substitute for these syscalls.
use super::super::{carrier, files, observer, refused, Deadline, QualifiedImage, Result};
use super::{pin, QualifiedBootstrap};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;

pub(super) fn status(pid: u32) -> Result<String> {
    let mut text = String::new();
    File::open(format!("/proc/{pid}/status"))
        .map_err(|_| refused())?
        .take(65537)
        .read_to_string(&mut text)
        .map_err(|_| refused())?;
    if text.len() > 65536 {
        return Err(refused());
    }
    Ok(text)
}
pub(super) fn field<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let mut fields = text.lines().filter_map(|l| l.strip_prefix(name));
    let value = fields.next().ok_or_else(refused)?.trim();
    if fields.next().is_some() {
        return Err(refused());
    }
    Ok(value)
}

/// No concurrent root/installer/recipient principal or capability-bearing peer
/// is admitted. Provider/system root processes must NOT be silently allowlisted
/// from guest literals. An elected provider without this isolation refuses.
fn exclusive(deadline: Deadline) -> Result<()> {
    let mut count = 0;
    for entry in std::fs::read_dir("/proc").map_err(|_| refused())? {
        deadline.check()?;
        let entry = entry.map_err(|_| refused())?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        count += 1;
        if count > 1024 {
            return Err(refused());
        }
        if pid == std::process::id() {
            continue;
        }
        // A process disappearing/appearing while sampling is UNKNOWN, not an
        // omitted principal. External mutable process sets cannot qualify.
        let before = crate::peer::proc_starttime(pid).ok_or_else(refused)?;
        let text = status(pid)?;
        for ids in ["Uid:", "Gid:"] {
            for id in field(&text, ids)?.split_ascii_whitespace() {
                let id: u32 = id.parse().map_err(|_| refused())?;
                if [0, 21000, 21001].contains(&id) {
                    return Err(refused());
                }
            }
        }
        for caps in ["CapEff:", "CapPrm:", "CapAmb:"] {
            if field(&text, caps)? != "0000000000000000" {
                return Err(refused());
            }
        }
        if crate::peer::proc_starttime(pid) != Some(before) {
            return Err(refused());
        }
    }
    deadline.check()
}
#[repr(C)]
struct MountAttr {
    set: u64,
    clear: u64,
    propagation: u64,
    userns_fd: u64,
}

pub(super) struct RootCustody {
    pub image: QualifiedImage,
    held: Vec<files::HeldArtifact>,
    root: File,
}
impl RootCustody {
    pub(super) fn acquire(b: &QualifiedBootstrap, deadline: Deadline) -> Result<Self> {
        if carrier::ids()? != [0; 6]
            || unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0
            || std::env::vars_os().next().is_some()
            || std::env::current_dir().map_err(|_| refused())? != std::path::Path::new("/")
        {
            return Err(refused());
        }
        let self_status = status(std::process::id())?;
        if field(&self_status, "Threads:")? != "1" || field(&self_status, "TracerPid:")? != "0" {
            return Err(refused());
        }
        if unsafe { libc::prctl(libc::PR_GET_SECUREBITS, 0, 0, 0, 0) } != 0
            || unsafe { libc::prctl(libc::PR_GET_KEEPCAPS, 0, 0, 0, 0) } != 0
        {
            return Err(refused());
        }
        exclusive(deadline)?;
        // Child mounts cannot affect the provider/other namespaces. Recursively
        // make the private tree immutable and suppress setuid/device escalation.
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
            return Err(refused());
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(refused());
        }
        let attrs = MountAttr {
            set: 1 | 2 | 4,
            clear: 0,
            propagation: 0,
            userns_fd: 0,
        };
        if unsafe {
            libc::syscall(
                libc::SYS_mount_setattr,
                libc::AT_FDCWD,
                c"/".as_ptr(),
                0x8000u32,
                &attrs as *const MountAttr,
                std::mem::size_of::<MountAttr>(),
            )
        } != 0
        {
            return Err(refused());
        }
        let a = &b.manifest.artifacts;
        let mut held = Vec::new();
        for (kind, digest) in [
            (files::Artifact::Constructor, &a.constructor),
            (files::Artifact::Carrier, &a.carrier),
            (files::Artifact::Observer, &a.observer),
        ] {
            held.push(files::HeldArtifact::open(kind, pin(digest)?, deadline)?);
        }
        held[0].self_correspondence(deadline)?;
        let root = File::open("/").map_err(|_| refused())?;
        let out = Self {
            image: QualifiedImage {
                client: pin(&a.client)?,
                carrier: pin(&a.carrier)?,
                observer: pin(&a.observer)?,
                namespaces: observer::capture_namespaces(deadline)?,
            },
            held,
            root,
        };
        out.recheck(deadline)?;
        Ok(out)
    }
    pub(super) fn recheck(&self, deadline: Deadline) -> Result<()> {
        deadline.check()?;
        if carrier::ids()? != [0; 6] {
            return Err(refused());
        }
        let text = status(std::process::id())?;
        if field(&text, "Threads:")? != "1" || field(&text, "TracerPid:")? != "0" {
            return Err(refused());
        }
        self.held[0].self_correspondence(deadline)?;
        let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(self.root.as_raw_fd(), &mut stats) } != 0
            || stats.f_flag & libc::ST_RDONLY == 0
            || stats.f_flag & libc::ST_NOSUID == 0
        {
            return Err(refused());
        }
        observer::self_namespaces(&self.image.namespaces, deadline)?;
        for artifact in &self.held {
            artifact.recheck(deadline)?;
        }
        Ok(())
    }
}
