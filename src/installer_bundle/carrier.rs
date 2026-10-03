//! Fixed non-setuid carrier: no fork, alternate action, args/env/cwd/FD knobs.
use super::{
    files::{Artifact, HeldArtifact},
    fixed_arguments, production_image, refused, seal, Deadline, Result, IDS,
};
use std::fs::File;
use std::os::fd::AsRawFd;

pub(super) fn ids() -> Result<[u32; 6]> {
    let mut u = [0; 3];
    let mut g = [0; 3];
    if unsafe { libc::getresuid(&mut u[0], &mut u[1], &mut u[2]) } != 0
        || unsafe { libc::getresgid(&mut g[0], &mut g[1], &mut g[2]) } != 0
    {
        return Err(refused());
    }
    Ok([u[0], u[1], u[2], g[0], g[1], g[2]])
}
fn fixed_stdio() -> Result<()> {
    for fd in 0..=2 {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut stat) } != 0
            || stat.st_mode & libc::S_IFMT != libc::S_IFIFO
        {
            return Err(refused());
        }
    }
    Ok(()) // owned provider pipes are still an unavailable external prerequisite
}

/// The only syscall policy. Fakes below assert order/refusal, not kernel seals.
trait Kernel {
    fn identities(&mut self) -> Result<[u32; 6]>;
    fn prepare(&mut self) -> Result<u32>;
    fn close_except(&mut self, fd: i32) -> Result<()>;
    fn groups(&mut self) -> Result<()>;
    fn gid(&mut self) -> Result<()>;
    fn uid(&mut self) -> Result<()>;
    fn finish(&mut self, last: u32) -> Result<()>;
    fn verify_groups(&mut self) -> Result<()>;
    fn cwd(&mut self) -> Result<()>;
    fn exec(&mut self, file: &File) -> Result<()>;
}
struct Linux {
    seal: seal::Linux,
    pin: [u8; 32],
    snapshot: Vec<super::files::Stamp>,
    deadline: Deadline,
}
impl Kernel for Linux {
    fn identities(&mut self) -> Result<[u32; 6]> {
        ids()
    }
    fn prepare(&mut self) -> Result<u32> {
        self.seal.prepare().map_err(|_| refused())
    }
    fn close_except(&mut self, fd: i32) -> Result<()> {
        if fd < 3 {
            return Err(refused());
        }
        if fd > 3
            && unsafe { libc::syscall(libc::SYS_close_range, 3u32, (fd - 1) as u32, 0u32) } != 0
        {
            return Err(refused());
        }
        if unsafe { libc::syscall(libc::SYS_close_range, (fd as u32) + 1, u32::MAX, 0u32) } != 0 {
            return Err(refused());
        }
        Ok(()) // no guessed FD cap or /proc enumeration fallback
    }
    fn groups(&mut self) -> Result<()> {
        if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 {
            return Err(refused());
        }
        Ok(())
    }
    fn gid(&mut self) -> Result<()> {
        if unsafe { libc::setresgid(IDS, IDS, IDS) } != 0 {
            return Err(refused());
        }
        Ok(())
    }
    fn uid(&mut self) -> Result<()> {
        if unsafe { libc::setresuid(IDS, IDS, IDS) } != 0 {
            return Err(refused());
        }
        Ok(())
    }
    fn finish(&mut self, last: u32) -> Result<()> {
        self.seal.finish(last).map_err(|_| refused())
    }
    fn verify_groups(&mut self) -> Result<()> {
        if unsafe { libc::getgroups(0, std::ptr::null_mut()) } != 0 {
            return Err(refused());
        }
        super::observer::sealed_self()
    }
    fn cwd(&mut self) -> Result<()> {
        if unsafe { libc::chdir(c"/".as_ptr()) } != 0 {
            return Err(refused());
        }
        Ok(())
    }
    fn exec(&mut self, file: &File) -> Result<()> {
        // Re-open and compare fixed topology plus held inode immediately after
        // seal/drop. New verification FDs close before exec, not after recycling.
        {
            let current = HeldArtifact::open(Artifact::Client, self.pin, self.deadline)?;
            if current.snapshot() != self.snapshot {
                return Err(refused());
            }
            super::files::correspond(file, current.selected_stamp())?;
        }
        self.deadline.check()?;
        // Static C strings/pointer arrays and owned held FD live across syscall.
        // CLOEXEC is safe for the elected ELF (never a shebang interpreter).
        let argv = [c"cadence-installer-client".as_ptr(), std::ptr::null()];
        let env: [*const libc::c_char; 1] = [std::ptr::null()];
        unsafe {
            libc::syscall(
                libc::SYS_execveat,
                file.as_raw_fd(),
                c"".as_ptr(),
                argv.as_ptr(),
                env.as_ptr(),
                libc::AT_EMPTY_PATH,
            );
        }
        Err(refused()) // exec never returns success; no fallback/PATH lookup
    }
}
fn drop_and_exec(kernel: &mut impl Kernel, file: &File, deadline: Deadline) -> Result<()> {
    deadline.check()?;
    if kernel.identities()? != [0; 6] {
        return Err(refused());
    }
    let last = kernel.prepare()?;
    kernel.close_except(file.as_raw_fd())?;
    kernel.groups()?;
    kernel.gid()?;
    kernel.uid()?;
    kernel.finish(last)?;
    if kernel.identities()? != [IDS; 6] {
        return Err(refused());
    }
    kernel.verify_groups()?;
    kernel.cwd()?;
    deadline.check()?;
    kernel.exec(file)
}
pub(super) fn entry() -> Result<()> {
    fixed_arguments()?;
    if ids()? != [0; 6] {
        return Err(refused());
    } // euid0 alone is insufficient
    let image = production_image()?; // no elected artifact/host bootstrap this batch
    fixed_stdio()?;
    let deadline = Deadline::new();
    let carrier = HeldArtifact::open(Artifact::Carrier, image.carrier, deadline)?;
    carrier.self_correspondence(deadline)?;
    drop(carrier);
    let held = HeldArtifact::open(Artifact::Client, image.client, deadline)?;
    held.recheck(deadline)?;
    let snapshot = held.snapshot().to_vec();
    let file = held.into_exec(); // directories closed before close_range
    drop_and_exec(
        &mut Linux {
            seal: seal::Linux,
            pin: image.client,
            snapshot,
            deadline,
        },
        &file,
        deadline,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        calls: Vec<&'static str>,
        fail: Option<usize>,
        sealed: bool,
        root: [u32; 6],
        fd: Option<i32>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                calls: Vec::new(),
                fail: None,
                sealed: false,
                root: [0; 6],
                fd: None,
            }
        }
        fn call(&mut self, name: &'static str) -> Result<()> {
            let i = self.calls.len();
            self.calls.push(name);
            if self.fail == Some(i) {
                return Err(refused());
            }
            Ok(())
        }
    }
    impl Kernel for Fake {
        fn identities(&mut self) -> Result<[u32; 6]> {
            self.call("ids")?;
            Ok(if self.sealed { [IDS; 6] } else { self.root })
        }
        fn prepare(&mut self) -> Result<u32> {
            self.call("prepare239")?;
            Ok(40)
        }
        fn close_except(&mut self, fd: i32) -> Result<()> {
            self.call("close-through-u32-max")?;
            self.fd = Some(fd);
            Ok(())
        }
        fn groups(&mut self) -> Result<()> {
            self.call("groups0")
        }
        fn gid(&mut self) -> Result<()> {
            self.call("setresgid21000")
        }
        fn uid(&mut self) -> Result<()> {
            self.call("setresuid21000")
        }
        fn finish(&mut self, last: u32) -> Result<()> {
            assert_eq!(last, 40);
            self.call("finish-empty-caps-nnp")?;
            self.sealed = true;
            Ok(())
        }
        fn verify_groups(&mut self) -> Result<()> {
            self.call("verify-groups")
        }
        fn cwd(&mut self) -> Result<()> {
            self.call("cwd-root")
        }
        fn exec(&mut self, file: &File) -> Result<()> {
            assert_eq!(self.fd, Some(file.as_raw_fd()));
            self.call("exec-held-same-pid")?;
            Err(refused())
        }
    }
    #[test]
    fn actual_unprivileged_close_range_keeps_selected_fd_and_closes_high_fd() {
        const FLAG: &str = "CAD1113_OWNED_FD_CHILD";
        if std::env::var_os(FLAG).is_some() {
            let held = tempfile::tempfile().unwrap();
            let high = unsafe { libc::fcntl(held.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 8192) };
            assert!(high >= 8192);
            let mut kernel = Linux {
                seal: seal::Linux,
                pin: [0; 32],
                snapshot: Vec::new(),
                deadline: Deadline::new(),
            };
            // ONLY this ordinary-UID child table: no caps/drop/exec/observer.
            kernel.close_except(held.as_raw_fd()).unwrap();
            assert!(unsafe { libc::fcntl(held.as_raw_fd(), libc::F_GETFD) } >= 0);
            assert_eq!(unsafe { libc::fcntl(high, libc::F_GETFD) }, -1);
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EBADF)
            );
            for fd in 0..=2 {
                assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
            }
            return;
        }
        let out=std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","installer_bundle::carrier::tests::actual_unprivileged_close_range_keeps_selected_fd_and_closes_high_fd","--test-threads=1"])
            .env(FLAG,"1").output().unwrap();
        assert!(
            out.status.success(),
            "owned FD child: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[test]
    fn carrier_model_exact_order_and_each_failure_never_reaches_exec() {
        let file = tempfile::tempfile().unwrap();
        let mut positive = Fake::new();
        assert!(drop_and_exec(&mut positive, &file, Deadline::new()).is_err());
        assert_eq!(
            positive.calls,
            [
                "ids",
                "prepare239",
                "close-through-u32-max",
                "groups0",
                "setresgid21000",
                "setresuid21000",
                "finish-empty-caps-nnp",
                "ids",
                "verify-groups",
                "cwd-root",
                "exec-held-same-pid"
            ]
        );
        for fail in 0..positive.calls.len() - 1 {
            let mut fake = Fake::new();
            fake.fail = Some(fail);
            assert!(drop_and_exec(&mut fake, &file, Deadline::new()).is_err());
            assert!(!fake.calls.contains(&"exec-held-same-pid"));
        }
        for index in 0..6 {
            let mut fake = Fake::new();
            fake.root[index] = 1000;
            assert!(drop_and_exec(&mut fake, &file, Deadline::new()).is_err());
            assert_eq!(fake.calls, ["ids"]);
        }
    }
}
