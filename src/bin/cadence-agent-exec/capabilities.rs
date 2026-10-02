//! Fail-closed capability sealing, independent of the UID transition.
//! Only the fixed root helper invokes the Linux implementation. Tests below
//! use a fake kernel and establish ordering/refusal, not platform protection.
use std::io;

pub const SECUREBITS: u32 = 239;
// Highest reviewed Linux capability: CAP_CHECKPOINT_RESTORE. New kernel
// capabilities require a policy review even though the v3 ABI can hold 64 bits.
const MAX_CAP: u32 = 40;

trait Kernel {
    fn last_cap(&mut self) -> io::Result<u32>;
    fn prctl(&mut self, option: i32, arg2: u32, arg3: u32) -> io::Result<i32>;
    fn clear_caps(&mut self) -> io::Result<()>;
    fn caps(&mut self) -> io::Result<[[u32; 3]; 2]>;
}

fn refusal() -> io::Error {
    io::Error::other("capability seal observation mismatch")
}

fn set(kernel: &mut impl Kernel, option: i32, arg2: u32, arg3: u32) -> io::Result<()> {
    if kernel.prctl(option, arg2, arg3)? != 0 {
        return Err(refusal());
    }
    Ok(())
}

fn before_drop(kernel: &mut impl Kernel) -> io::Result<u32> {
    let last = kernel.last_cap()?;
    if last > MAX_CAP {
        return Err(refusal());
    }
    set(
        kernel,
        libc::PR_CAP_AMBIENT,
        libc::PR_CAP_AMBIENT_CLEAR_ALL as u32,
        0,
    )?;
    // Bounding-set removal needs CAP_SETPCAP before setuid/capset. It does not
    // itself empty the current effective/permitted sets required for ID drop.
    for cap in 0..=last {
        set(kernel, libc::PR_CAPBSET_DROP, cap, 0)?;
    }
    // NOROOT+lock, NO_SETUID_FIXUP+lock, KEEP_CAPS off+lock, ambient raise off+lock.
    set(kernel, libc::PR_SET_SECUREBITS, SECUREBITS, 0)?;
    Ok(last)
}

fn after_drop(kernel: &mut impl Kernel, last: u32) -> io::Result<()> {
    if last > MAX_CAP || kernel.last_cap()? != last {
        return Err(refusal());
    }
    // NO_SETUID_FIXUP deliberately makes the UID transition insufficient.
    // Empty both v3 words explicitly, including inheritable capabilities.
    kernel.clear_caps()?;
    set(kernel, libc::PR_SET_NO_NEW_PRIVS, 1, 0)?;
    if kernel.caps()? != [[0; 3]; 2]
        || kernel.prctl(libc::PR_GET_SECUREBITS, 0, 0)? != SECUREBITS as i32
        || kernel.prctl(libc::PR_GET_KEEPCAPS, 0, 0)? != 0
        || kernel.prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0)? != 1
    {
        return Err(refusal());
    }
    for cap in 0..=last {
        if kernel.prctl(libc::PR_CAPBSET_READ, cap, 0)? != 0
            || kernel.prctl(
                libc::PR_CAP_AMBIENT,
                libc::PR_CAP_AMBIENT_IS_SET as u32,
                cap,
            )? != 0
        {
            return Err(refusal());
        }
    }
    Ok(())
}

#[repr(C)]
struct Header {
    version: u32,
    pid: i32,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Data {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

pub struct Linux;
impl Kernel for Linux {
    fn last_cap(&mut self) -> io::Result<u32> {
        use std::io::Read;
        let mut value = String::new();
        std::fs::File::open("/proc/sys/kernel/cap_last_cap")?
            .take(17)
            .read_to_string(&mut value)?;
        if value.len() > 16 {
            return Err(refusal());
        }
        let digits = value.strip_suffix('\n').unwrap_or(&value);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(refusal());
        }
        digits.parse().map_err(|_| refusal())
    }
    fn prctl(&mut self, option: i32, arg2: u32, arg3: u32) -> io::Result<i32> {
        let result = unsafe {
            libc::prctl(
                option,
                libc::c_ulong::from(arg2),
                libc::c_ulong::from(arg3),
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }
    fn clear_caps(&mut self) -> io::Result<()> {
        let header = Header {
            version: 0x2008_0522,
            pid: 0,
        };
        let data = [Data::default(); 2];
        if unsafe { libc::syscall(libc::SYS_capset, &header as *const Header, data.as_ptr()) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    fn caps(&mut self) -> io::Result<[[u32; 3]; 2]> {
        let mut header = Header {
            version: 0x2008_0522,
            pid: 0,
        };
        let mut data = [Data::default(); 2];
        if unsafe {
            libc::syscall(
                libc::SYS_capget,
                &mut header as *mut Header,
                data.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(data.map(|d| [d.effective, d.permitted, d.inheritable]))
    }
}

impl Linux {
    pub fn prepare(&mut self) -> io::Result<u32> {
        before_drop(self)
    }
    pub fn finish(&mut self, last: u32) -> io::Result<()> {
        after_drop(self, last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake {
        last: u32,
        calls: usize,
        fail_at: Option<usize>,
        bounding: [u32; 64],
        ambient: [u32; 64],
        securebits: i32,
        keepcaps: i32,
        nnp: i32,
        caps: [[u32; 3]; 2],
        ignore_clear: bool,
        remaining_caps: [[u32; 3]; 2],
        ignore_nnp: bool,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                last: 40,
                calls: 0,
                fail_at: None,
                bounding: [1; 64],
                ambient: [1; 64],
                securebits: 0,
                keepcaps: 0,
                nnp: 0,
                caps: [[1; 3]; 2],
                ignore_clear: false,
                remaining_caps: [[0; 3]; 2],
                ignore_nnp: false,
            }
        }
    }
    impl Fake {
        fn call(&mut self) -> io::Result<()> {
            let current = self.calls;
            self.calls += 1;
            if self.fail_at == Some(current) {
                Err(refusal())
            } else {
                Ok(())
            }
        }
    }
    impl Kernel for Fake {
        fn last_cap(&mut self) -> io::Result<u32> {
            self.call()?;
            Ok(self.last)
        }
        fn prctl(&mut self, option: i32, a: u32, b: u32) -> io::Result<i32> {
            self.call()?;
            match option {
                libc::PR_CAP_AMBIENT if a == libc::PR_CAP_AMBIENT_CLEAR_ALL as u32 => {
                    self.ambient = [0; 64];
                    Ok(0)
                }
                libc::PR_CAP_AMBIENT => Ok(self.ambient[b as usize] as i32),
                libc::PR_CAPBSET_DROP => {
                    self.bounding[a as usize] = 0;
                    Ok(0)
                }
                libc::PR_CAPBSET_READ => Ok(self.bounding[a as usize] as i32),
                libc::PR_SET_SECUREBITS => {
                    self.securebits = a as i32;
                    Ok(0)
                }
                libc::PR_GET_SECUREBITS => Ok(self.securebits),
                libc::PR_GET_KEEPCAPS => Ok(self.keepcaps),
                libc::PR_SET_NO_NEW_PRIVS => {
                    if !self.ignore_nnp {
                        self.nnp = a as i32;
                    }
                    Ok(0)
                }
                libc::PR_GET_NO_NEW_PRIVS => Ok(self.nnp),
                _ => Err(refusal()),
            }
        }
        fn clear_caps(&mut self) -> io::Result<()> {
            self.call()?;
            if !self.ignore_clear {
                self.caps = self.remaining_caps;
            }
            Ok(())
        }
        fn caps(&mut self) -> io::Result<[[u32; 3]; 2]> {
            self.call()?;
            Ok(self.caps)
        }
    }
    #[test]
    fn capability_seal_orders_pre_drop_bounds_and_post_drop_explicit_empty() {
        let mut kernel = Fake::default();
        let last = before_drop(&mut kernel).unwrap();
        assert_eq!(
            kernel.caps, [[1; 3]; 2],
            "ID-drop authority not cleared prematurely"
        );
        assert_eq!(kernel.securebits, 239);
        after_drop(&mut kernel, last).unwrap();
        assert_eq!(kernel.caps, [[0; 3]; 2]);
        assert_eq!(kernel.nnp, 1);
    }
    #[test]
    fn every_capability_syscall_failure_refuses_before_any_verb() {
        let mut reference = Fake::default();
        let last = before_drop(&mut reference).unwrap();
        after_drop(&mut reference, last).unwrap();
        for failure in 0..reference.calls {
            let mut kernel = Fake {
                fail_at: Some(failure),
                ..Fake::default()
            };
            let result = before_drop(&mut kernel).and_then(|last| after_drop(&mut kernel, last));
            assert!(result.is_err(), "failure {failure} was ignored");
        }
    }
    #[test]
    fn unsupported_or_changed_kernel_capability_range_refuses() {
        let mut kernel = Fake {
            last: 41,
            ..Fake::default()
        };
        assert!(before_drop(&mut kernel).is_err());
        let mut kernel = Fake::default();
        let last = before_drop(&mut kernel).unwrap();
        kernel.last = 41;
        assert!(after_drop(&mut kernel, last).is_err());
    }
    #[test]
    fn residual_capability_authority_or_unlocked_bits_refuses() {
        for mode in 0..6 {
            let mut kernel = Fake::default();
            let last = before_drop(&mut kernel).unwrap();
            match mode {
                0 => kernel.ignore_clear = true,
                1 => kernel.securebits = 0,
                2 => kernel.keepcaps = 1,
                3 => kernel.bounding[40] = 1,
                4 => kernel.ambient[40] = 1,
                _ => kernel.ignore_nnp = true,
            }
            assert!(after_drop(&mut kernel, last).is_err());
        }
    }
    #[test]
    fn every_v3_capability_word_and_set_must_be_zero() {
        for word in 0..2 {
            for set in 0..3 {
                let mut kernel = Fake::default();
                let last = before_drop(&mut kernel).unwrap();
                kernel.remaining_caps[word][set] = 1;
                assert!(after_drop(&mut kernel, last).is_err());
            }
        }
    }
}
