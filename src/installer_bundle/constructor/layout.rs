//! Fixed mutable enclaves in the constructor's PRIVATE mount namespace. No
//! caller path, snapshot marker or broad root-RW remount is accepted.
use super::super::{refused, Deadline, Result};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
#[repr(C)]
struct Attr {
    set: u64,
    clear: u64,
    propagation: u64,
    userns_fd: u64,
}
struct Mount {
    path: &'static str,
    file: File,
    dev: u64,
    ino: u64,
    mount_id: u64,
}
pub(super) struct Layout {
    mounts: Vec<Mount>,
}
fn mount_id(file: &File) -> Result<u64> {
    use std::io::Read;
    let mut text = String::new();
    File::open(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))
        .map_err(|_| refused())?
        .take(8193)
        .read_to_string(&mut text)
        .map_err(|_| refused())?;
    if text.len() > 8192 {
        return Err(refused());
    }
    let mut ids = text.lines().filter_map(|line| {
        line.strip_prefix("mnt_id:")
            .and_then(|s| s.trim().parse::<u64>().ok())
    });
    let id = ids.next().ok_or_else(refused)?;
    if ids.next().is_some() {
        return Err(refused());
    }
    Ok(id)
}
fn directory(path: &str) -> Result<File> {
    let mut prefix = std::path::PathBuf::new();
    for part in Path::new(path).components() {
        prefix.push(part);
        let meta = std::fs::symlink_metadata(&prefix).map_err(|_| refused())?;
        let shared = crate::adapter::pi_guest::topology::resolve_gid_pub(
            crate::adapter::pi_guest::acct::SHARED_GROUP,
        )?;
        if shared == 0 {
            return Err(refused());
        }
        let (owner, gid, mode) = if [
            Path::new("/srv/cadence/protected"),
            Path::new("/srv/cadence/protected/store"),
            Path::new("/run/cadence/private"),
        ]
        .contains(&prefix.as_path())
        {
            (21000, 21000, 0o700)
        } else if [
            Path::new("/srv/cadence/guest-views"),
            Path::new("/workspace"),
        ]
        .contains(&prefix.as_path())
        {
            (21000, shared, 0o750)
        } else {
            (0, 0, 0o755)
        };
        if !meta.is_dir()
            || meta.uid() != owner
            || meta.gid() != gid
            || meta.mode() & 0o7777 != mode
        {
            return Err(refused());
        }
    }
    File::open(path).map_err(|_| refused())
}
impl Layout {
    pub(super) fn acquire(deadline: Deadline) -> Result<Self> {
        let mut mounts = Vec::new();
        for path in [
            "/srv/cadence/protected/store",
            "/srv/cadence/guest-views",
            "/workspace",
            "/run/cadence/private",
        ] {
            deadline.check()?;
            let file = directory(path)?;
            let meta = file.metadata().map_err(|_| refused())?;
            let name = std::ffi::CString::new(path).map_err(|_| refused())?;
            // Exact self-bind creates an enclave mount. Clearing RDONLY on a
            // directory within the root mount would otherwise unlock ROOT.
            if unsafe {
                libc::mount(
                    name.as_ptr(),
                    name.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            } != 0
            {
                return Err(refused());
            }
            let attrs = Attr {
                set: 2 | 4,
                clear: 1,
                propagation: 0,
                userns_fd: 0,
            };
            if unsafe {
                libc::syscall(
                    libc::SYS_mount_setattr,
                    libc::AT_FDCWD,
                    name.as_ptr(),
                    0u32,
                    &attrs as *const Attr,
                    std::mem::size_of::<Attr>(),
                )
            } != 0
            {
                return Err(refused());
            }
            let reopened = directory(path)?;
            let after = reopened.metadata().map_err(|_| refused())?;
            if (meta.dev(), meta.ino()) != (after.dev(), after.ino()) {
                return Err(refused());
            }
            let selected_mount = mount_id(&reopened)?;
            mounts.push(Mount {
                path,
                file: reopened,
                dev: meta.dev(),
                ino: meta.ino(),
                mount_id: selected_mount,
            });
        }
        let layout = Self { mounts };
        layout.recheck(deadline)?;
        Ok(layout)
    }
    /// Corroborate an actual held/named Store file against the originally
    /// retained mutable mount. Metadata is not creation/permit authority.
    pub(super) fn store_file(&self, file: &File) -> Result<()> {
        let store = self
            .mounts
            .iter()
            .find(|m| m.path == "/srv/cadence/protected/store")
            .ok_or_else(refused)?;
        let meta = file.metadata().map_err(|_| refused())?;
        if meta.dev() != store.dev
            || mount_id(file)? != store.mount_id
            || mount_id(&store.file)? != store.mount_id
        {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn recheck(&self, deadline: Deadline) -> Result<()> {
        for m in &self.mounts {
            deadline.check()?;
            let current = directory(m.path)?;
            let meta = current.metadata().map_err(|_| refused())?;
            let held = m.file.metadata().map_err(|_| refused())?;
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            if (meta.dev(), meta.ino()) != (m.dev, m.ino)
                || (held.dev(), held.ino()) != (m.dev, m.ino)
                || mount_id(&current)? != m.mount_id
                || mount_id(&m.file)? != m.mount_id
                || unsafe { libc::fstatvfs(current.as_raw_fd(), &mut stat) } != 0
                || stat.f_flag & libc::ST_RDONLY != 0
                || stat.f_flag & libc::ST_NOSUID == 0
                || stat.f_flag & libc::ST_NODEV == 0
            {
                return Err(refused());
            }
        }
        Ok(())
    }
}
