//! Bounded NDJSON duplex, shared by provider streams and the fixed inherited
//! private owner channel. Reading a frame does not consume EOF or a later frame.
use super::super::{refused, Deadline, HostFrame, Result};
use std::os::fd::RawFd;
const MAX_FRAME: usize = 65536;
const MAX_FRAMES: usize = 32;
const MAX_TOTAL: usize = 262144;

fn ready(fd: RawFd, events: i16, deadline: Deadline) -> Result<()> {
    loop {
        deadline.check()?;
        let ms = deadline
            .0
            .saturating_duration_since(std::time::Instant::now())
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut p = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut p, 1, ms) };
        deadline.check()?;
        if rc > 0 && p.revents & events != 0 {
            return Ok(());
        }
        if rc < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(refused());
    }
}
pub(super) fn nonblocking(fd: RawFd) -> Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(refused());
    }
    Ok(())
}
fn interrupted() -> bool {
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EINTR) | Some(libc::EAGAIN)
    )
}
fn valid(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && bytes.len() < MAX_FRAME
        && bytes.iter().all(|b| b.is_ascii() && !b"\n\r\0".contains(b))
}

/// Descriptors originate only from the provider entry or constructor-owned
/// socketpairs, never from JSON. Counts span both directions for one operation.
pub(super) struct Duplex {
    input: RawFd,
    output: RawFd,
    pending: HostFrame,
    frames: usize,
    total: usize,
    deadline: Deadline,
    failed: bool,
}
impl Duplex {
    pub(super) fn new(input: RawFd, output: RawFd, deadline: Deadline) -> Result<Self> {
        nonblocking(input)?;
        nonblocking(output)?;
        Ok(Self {
            input,
            output,
            pending: HostFrame(Vec::new()),
            frames: 0,
            total: 0,
            deadline,
            failed: false,
        })
    }
    fn count(&mut self, length: usize) -> Result<()> {
        self.deadline.check()?;
        if self.failed
            || length >= MAX_FRAME
            || self.frames >= MAX_FRAMES
            || self
                .total
                .checked_add(length + 1)
                .is_none_or(|n| n > MAX_TOTAL)
        {
            return Err(refused());
        }
        self.frames += 1;
        self.total += length + 1;
        Ok(())
    }
    pub(super) fn receive(&mut self) -> Result<HostFrame> {
        let result = self.receive_once();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn receive_once(&mut self) -> Result<HostFrame> {
        if self.failed {
            return Err(refused());
        }
        loop {
            self.deadline.check()?;
            if let Some(end) = self.pending.0.iter().position(|b| *b == b'\n') {
                if !valid(&self.pending.0[..end]) {
                    return Err(refused());
                }
                self.count(end)?;
                let frame = HostFrame(self.pending.0[..end].to_vec());
                self.pending.0[..end + 1].fill(0);
                self.pending.0.drain(..end + 1);
                return Ok(frame);
            }
            if self.pending.0.len() >= MAX_FRAME {
                return Err(refused());
            }
            ready(self.input, libc::POLLIN, self.deadline)?;
            let mut chunk = [0u8; 4096];
            let want = chunk.len().min(MAX_FRAME - self.pending.0.len());
            let n = unsafe { libc::read(self.input, chunk.as_mut_ptr().cast(), want) };
            if n < 0 && interrupted() {
                continue;
            }
            if n <= 0 {
                return Err(refused());
            } // no EOF-as-frame or retry
            self.pending.0.extend_from_slice(&chunk[..n as usize]);
            chunk.fill(0);
        }
    }
    pub(super) fn send(&mut self, bytes: &[u8]) -> Result<()> {
        let result = self.send_once(bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn send_once(&mut self, bytes: &[u8]) -> Result<()> {
        if !valid(bytes) {
            return Err(refused());
        }
        self.count(bytes.len())?; // budget consumed even if a partial write fails
        let mut frame = HostFrame(bytes.to_vec());
        frame.0.push(b'\n');
        let mut offset = 0;
        while offset < frame.0.len() {
            ready(self.output, libc::POLLOUT, self.deadline)?;
            let n = unsafe {
                libc::write(
                    self.output,
                    frame.0[offset..].as_ptr().cast(),
                    frame.0.len() - offset,
                )
            };
            self.deadline.check()?;
            if n < 0 && interrupted() {
                continue;
            }
            if n <= 0 {
                return Err(refused());
            }
            offset += n as usize;
        }
        Ok(())
    }
}
