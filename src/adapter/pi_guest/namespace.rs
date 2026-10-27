//! Permanent inherited namespace/principal confinement for Node and tool children.
//! Empty capabilities + NNP alone do not stop CLONE_NEWUSER reacquisition.
use std::io;
fn stmt(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn jump(code: u16, k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter { code, jt, jf, k }
}
/// Called in the single-threaded sealed helper. Filter inheritance has no opt-out;
/// clone3 returns ENOSYS so supported Node/glibc thread creation can fall back to
/// clone without namespace flags. No syscall silently downgrades confinement.
pub(crate) fn install() -> io::Result<()> {
    #[cfg(target_arch = "x86_64")]
    let arch = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    let arch = 0xc00000b7;
    const LD: u16 = 0x20;
    const EQ: u16 = 0x15;
    const SET: u16 = 0x45;
    const RET: u16 = 0x06;
    const KILL: u32 = 0x80000000;
    const ALLOW: u32 = 0x7fff0000;
    const ERRNO: u32 = 0x00050000;
    let mut filter = vec![
        stmt(LD, 4),
        jump(EQ, arch, 1, 0),
        stmt(RET, KILL),
        stmt(LD, 0),
    ];
    // Also refuses x32 syscall aliases on x86_64 by allowing only native nr.
    #[cfg(target_arch = "x86_64")]
    filter.extend([jump(SET, 0x40000000, 0, 1), stmt(RET, KILL)]);
    for nr in [
        libc::SYS_unshare,
        libc::SYS_setns,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_pivot_root,
        libc::SYS_chroot,
        libc::SYS_ptrace,
        libc::SYS_process_vm_writev,
        libc::SYS_open_by_handle_at,
        libc::SYS_move_mount,
        libc::SYS_mount_setattr,
    ] {
        filter.extend([
            jump(EQ, nr as u32, 0, 1),
            stmt(RET, ERRNO | libc::EPERM as u32),
        ]);
    }
    filter.extend([
        jump(EQ, libc::SYS_clone3 as u32, 0, 1),
        stmt(RET, ERRNO | libc::ENOSYS as u32),
    ]);
    let namespaces = (libc::CLONE_NEWUSER
        | libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWCGROUP) as u32;
    filter.extend([
        jump(EQ, libc::SYS_clone as u32, 0, 3),
        stmt(LD, 16),
        jump(SET, namespaces, 0, 1),
        stmt(RET, ERRNO | libc::EPERM as u32),
        stmt(RET, ALLOW),
    ]);
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
        || unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
