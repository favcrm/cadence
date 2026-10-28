//! The bounded owned-console transport for one already-started runtime.
//!
//! OCI start is not chat. The root owner adds exactly one finite attachment
//! for an owned runtime (`open_console_v1`): it attaches to that container's
//! Pi stdin/stdout/stderr through Docker's fixed attach API, checks that the
//! accepting peer is the enrolled supervisor process (repeat of the control
//! exchange's pid/starttime plus a one-use token), and demultiplexes Docker's
//! raw stream into two typed channels. Docker's attach framing and stdout/
//! stderr demultiplexing stay owner-internal; the supervisor only sees the
//! small closed console framing below.
//!
//! Console wire (supervisor <-> owner), after one strict JSON hello line:
//!
//! ```text
//! owner -> supervisor : [channel:u8][len:u32be][payload]   channel 1 stdout, 2 stderr
//! supervisor -> owner : raw bytes                          the guest's stdin
//! ```
//!
//! There is no caller-selected target: the path is derived from the owner's
//! own run directory and the obligation it already holds. EOF/error closes
//! that attachment only — it never stops the container, never resends a
//! prompt, and never becomes a second authority.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::strict;

/// Channel tags in the owner -> supervisor direction.
pub(crate) const CHANNEL_STDOUT: u8 = 1;
pub(crate) const CHANNEL_STDERR: u8 = 2;

/// One console payload is at most 64 KiB, so a guest that floods stdout cannot
/// make either side allocate without bound.
pub(crate) const MAX_CONSOLE_PAYLOAD: usize = 64 * 1024;
/// The hello line is tiny and closed.
const MAX_HELLO_BYTES: usize = 512;
/// How long the supervisor's stderr projection may grow before it is dropped.
/// stderr is diagnostics, never protocol.
const MAX_STDERR_BYTES: u64 = 8 * 1024 * 1024;
/// How long the owner waits for the supervisor to attach after `open_console`.
const ATTACH_DEADLINE: Duration = Duration::from_secs(30);
/// Console read/write deadlines on the supervisor side. A silent peer is a
/// closed attachment, never an unbounded wait.
const CONSOLE_IO_DEADLINE: Duration = Duration::from_secs(30);

/// Write one owner -> supervisor console frame.
pub(crate) fn write_channel(stream: &mut UnixStream, channel: u8, payload: &[u8]) -> Result<()> {
    if !matches!(channel, CHANNEL_STDOUT | CHANNEL_STDERR) || payload.len() > MAX_CONSOLE_PAYLOAD {
        return Err(Error::internal(
            "console frame is not a bounded typed channel",
        ));
    }
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(channel);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(payload);
    stream
        .write_all(&frame)
        .map_err(|_| Error::unknown("owned console attachment closed"))
}

/// Read one owner -> supervisor console frame.
pub(crate) fn read_channel(stream: &mut UnixStream) -> Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    stream
        .read_exact(&mut header)
        .map_err(|_| Error::unknown("owned console attachment closed"))?;
    let channel = header[0];
    if !matches!(channel, CHANNEL_STDOUT | CHANNEL_STDERR) {
        return Err(Error::rejected("console frame names an unknown channel"));
    }
    let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if len > MAX_CONSOLE_PAYLOAD {
        return Err(Error::rejected("console frame exceeds the payload bound"));
    }
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .map_err(|_| Error::unknown("owned console attachment closed"))?;
    Ok((channel, payload))
}

// ---------------------------------------------------------------------------
// Owner side
// ---------------------------------------------------------------------------

/// What the owner already proved before it may bind the console listener.
pub(crate) struct ConsoleGrant {
    pub(crate) supervisor_uid: u32,
    pub(crate) supervisor_pid: u32,
    pub(crate) supervisor_starttime: u64,
    pub(crate) runtime_id: String,
    pub(crate) generation: u64,
    /// One-use attachment token minted by the owner for this open.
    pub(crate) token: String,
}

/// Bind the per-runtime console socket and start the accepting thread.
///
/// The socket lives beside (never inside) the guest-visible operations path:
/// the guest gets only the single `ops.sock` file mount, so this sibling file
/// is invisible to it. The directory is the owner's root-owned runtime
/// directory; the socket is `0660 root:<supervisor_gid>` so only the enrolled
/// supervisor account can connect, and every accepted peer is re-measured.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_owner(
    path: &Path,
    supervisor_gid: u32,
    grant: ConsoleGrant,
    attach: UnixStream,
) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    if let Ok(stale) = std::fs::symlink_metadata(path) {
        use std::os::unix::fs::FileTypeExt;
        if !stale.file_type().is_socket() || stale.uid() != 0 {
            return Err(Error::rejected(
                "console path is not a stale root-owned console socket",
            ));
        }
        std::fs::remove_file(path)
            .map_err(|_| Error::internal("stale console socket cannot be replaced"))?;
    }
    let listener =
        UnixListener::bind(path).map_err(|_| Error::internal("console socket cannot be bound"))?;
    let mut perms = std::fs::metadata(path)
        .map_err(|_| Error::internal("console socket metadata unreadable"))?
        .permissions();
    perms.set_mode(0o660);
    std::fs::set_permissions(path, perms)
        .map_err(|_| Error::internal("console socket mode cannot be set"))?;
    super::spec::chown_path(path, 0, supervisor_gid)?;
    std::thread::spawn(move || {
        let _ = serve_console(listener, grant, attach);
    });
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn spawn_owner(
    _path: &Path,
    _supervisor_gid: u32,
    _grant: ConsoleGrant,
    _attach: UnixStream,
) -> Result<()> {
    Err(Error::rejected(
        "the owned OCI console transport is Linux-only",
    ))
}

#[cfg(target_os = "linux")]
fn serve_console(listener: UnixListener, grant: ConsoleGrant, attach: UnixStream) -> Result<()> {
    let deadline = Instant::now() + ATTACH_DEADLINE;
    let mut console = accept_until(&listener, deadline)?;
    // The peer is re-measured on attachment: the control exchange's measured
    // pid/starttime must be the same live process, on the supervisor uid.
    let (uid, pid) = super::peer::credentials(&console)?;
    let starttime = super::peer::starttime(pid)
        .ok_or_else(|| Error::rejected("console peer starttime unreadable"))?;
    if uid != grant.supervisor_uid
        || pid != grant.supervisor_pid
        || starttime != grant.supervisor_starttime
    {
        return Err(Error::rejected(
            "console peer is not the enrolled supervisor process",
        ));
    }
    console
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|_| Error::rejected("console read deadline unavailable"))?;
    let hello = read_line(&mut console, MAX_HELLO_BYTES)?;
    let value = strict::parse(&hello)?;
    let m = strict::object(&value, &["v", "runtime_id", "generation", "token"])?;
    if strict::safe_u64(m, "v")? != 1
        || strict::string(m, "runtime_id")? != grant.runtime_id.as_str()
        || strict::safe_u64(m, "generation")? != grant.generation
        || strict::string(m, "token")? != grant.token.as_str()
    {
        return Err(Error::rejected(
            "console hello does not bind this exact attachment",
        ));
    }
    console
        .set_read_timeout(None)
        .map_err(|_| Error::rejected("console read deadline cannot be cleared"))?;
    console
        .set_write_timeout(Some(CONSOLE_IO_DEADLINE))
        .map_err(|_| Error::rejected("console write deadline unavailable"))?;

    let mut console_reader = console
        .try_clone()
        .map_err(|_| Error::internal("console reader cannot be cloned"))?;
    let mut attach_reader = attach
        .try_clone()
        .map_err(|_| Error::internal("attach reader cannot be cloned"))?;
    let mut console_writer = console;
    let mut attach_writer = attach;
    // Docker -> console: read the raw stream's 8-byte frames and emit our
    // bounded typed channels.
    let to_console = std::thread::spawn(move || {
        let _ = pump_docker(&mut attach_reader, &mut console_writer);
    });
    // console -> Docker: the guest's stdin is raw bytes on the hijacked
    // connection.
    let to_docker = std::thread::spawn(move || {
        let mut chunk = vec![0u8; MAX_CONSOLE_PAYLOAD];
        loop {
            match console_reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if attach_writer.write_all(&chunk[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = attach_writer.shutdown(std::net::Shutdown::Both);
    });
    let _ = to_console.join();
    let _ = to_docker.join();
    Ok(())
}

#[cfg(target_os = "linux")]
fn accept_until(listener: &UnixListener, deadline: Instant) -> Result<UnixStream> {
    use std::os::unix::io::AsRawFd;
    listener
        .set_nonblocking(true)
        .map_err(|_| Error::unknown("console listener nonblocking failed"))?;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .map_err(|_| Error::internal("console stream blocking failed"))?;
                return Ok(stream);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(Error::unknown("console attachment deadline elapsed"));
                }
                let mut poll = libc::pollfd {
                    fd: listener.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe { libc::poll(&mut poll, 1, 200) };
            }
            Err(_) => return Err(Error::unknown("console accept failed")),
        }
    }
}

#[cfg(target_os = "linux")]
fn pump_docker(docker: &mut UnixStream, console: &mut UnixStream) -> Result<()> {
    let mut header = [0u8; 8];
    loop {
        if docker.read_exact(&mut header).is_err() {
            break;
        }
        // Docker's hijacked attach header is `[STREAM, 0, 0, 0, SIZE_BE32]`:
        // the stream type is the first byte, not a 32-bit big-endian word.
        // Reading it as a u32 turns stdout (1) into 0x01000000 and drops every
        // guest response.
        let stream = header[0];
        let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let channel = match stream {
            1 => CHANNEL_STDOUT,
            2 => CHANNEL_STDERR,
            _ => {
                return Err(Error::rejected(
                    "docker attach frame names an unknown stream",
                ))
            }
        };
        let mut remaining = size;
        // A Docker frame may exceed our console payload bound; split it into
        // bounded console frames instead of allocating the whole frame.
        let mut buffer = vec![0u8; remaining.min(MAX_CONSOLE_PAYLOAD)];
        while remaining > 0 {
            let want = remaining.min(MAX_CONSOLE_PAYLOAD);
            if docker.read_exact(&mut buffer[..want]).is_err() {
                return Err(Error::unknown("docker attach stream truncated"));
            }
            write_channel(console, channel, &buffer[..want])?;
            remaining -= want;
        }
    }
    Ok(())
}

/// Read one newline-terminated line with a hard byte bound.
fn read_line(stream: &mut UnixStream, max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(64);
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return Err(Error::rejected("console hello closed without a newline")),
            Ok(_) => {
                if byte[0] == b'\n' {
                    return Ok(out);
                }
                out.push(byte[0]);
                if out.len() > max {
                    return Err(Error::rejected("console hello exceeds the bound"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(Error::unknown("console hello deadline elapsed"))
            }
            Err(_) => return Err(Error::rejected("console hello read failed")),
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor side
// ---------------------------------------------------------------------------

/// A live attachment to one owned runtime's console. The guest's stdin is
/// [`Self::stdin`]; [`Self::stdout`] is a plain byte stream the Pi transport
/// reads. stderr is projected to the provider log (bounded, diagnostics only)
/// and never becomes protocol.
pub(crate) struct OwnerConsole {
    stdin: Option<UnixStream>,
    stdout: Option<UnixStream>,
    reader: Option<std::thread::JoinHandle<()>>,
    writer: Arc<AtomicBool>,
    pid: u32,
    detached: bool,
}

impl OwnerConsole {
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }

    pub(crate) fn write_stdin(&self, bytes: &[u8]) -> std::io::Result<()> {
        let Some(stream) = self.stdin.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "console closed",
            ));
        };
        let mut stream = stream.try_clone()?;
        stream.write_all(bytes)?;
        stream.flush()
    }

    pub(crate) fn stdin_raw_fd(&self) -> Option<std::os::unix::io::RawFd> {
        use std::os::unix::io::AsRawFd;
        self.stdin.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(crate) fn take_stdout(&mut self) -> Option<UnixStream> {
        self.stdout.take()
    }

    pub(crate) fn close_stdin(&mut self) {
        if let Some(stream) = self.stdin.take() {
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    }

    pub(crate) fn close(&mut self) {
        self.writer.store(true, Ordering::SeqCst);
        self.close_stdin();
        if let Some(stream) = self.stdout.take() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(handle) = self.reader.take() {
            // Bounded best effort: never let a stuck peer hold a shutdown.
            let deadline = Instant::now() + Duration::from_millis(500);
            while !handle.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

impl Drop for OwnerConsole {
    fn drop(&mut self) {
        if !self.detached {
            self.close();
        }
    }
}

/// The two plain byte streams of one attachment plus the measured guest pid.
/// Taking them detaches the adapter's demultiplexer: it runs until the console
/// socket closes, and the transport owns the lifecycle from then on.
pub(crate) struct ConsoleParts {
    pub(crate) stdin: UnixStream,
    pub(crate) stdout: UnixStream,
    pub(crate) pid: u32,
}

impl OwnerConsole {
    pub(crate) fn into_parts(mut self) -> Result<ConsoleParts> {
        let stdin = self
            .stdin
            .take()
            .ok_or_else(|| Error::internal("console stdin is already taken"))?;
        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| Error::internal("console stdout is already taken"))?;
        let pid = self.pid;
        self.detached = true;
        Ok(ConsoleParts { stdin, stdout, pid })
    }
}

/// Connect to the owner's console listener and start the typed demultiplexer.
/// `path`/`token` came from the authenticated `open_console_v1` reply and are
/// used as selectors only; the owner re-measures this process on accept.
pub(crate) fn connect(
    path: &Path,
    token: &str,
    runtime_id: &str,
    generation: u64,
    pid: u32,
    stderr_log: Option<PathBuf>,
) -> Result<OwnerConsole> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut console = loop {
        match UnixStream::connect(path) {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                return Err(Error::unknown(
                    "owned console listener is unreachable — no attachment",
                ))
            }
        }
    };
    console
        .set_write_timeout(Some(CONSOLE_IO_DEADLINE))
        .map_err(|_| Error::rejected("console write deadline unavailable"))?;
    let hello = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "runtime_id": runtime_id,
        "generation": generation,
        "token": token,
    }))
    .map_err(|_| Error::internal("console hello cannot be encoded"))?;
    let mut payload = hello;
    payload.push(b'\n');
    console
        .write_all(&payload)
        .map_err(|_| Error::unknown("owned console hello was refused"))?;
    console
        .set_read_timeout(Some(CONSOLE_IO_DEADLINE))
        .map_err(|_| Error::rejected("console read deadline unavailable"))?;
    let reader_stream = console
        .try_clone()
        .map_err(|_| Error::internal("console reader cannot be cloned"))?;
    let (stdout_read, stdout_write) =
        UnixStream::pair().map_err(|_| Error::internal("console stdout pipe cannot be created"))?;
    let closed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&closed);
    let reader = std::thread::spawn(move || {
        let mut stream = reader_stream;
        let mut writer = stdout_write;
        let mut stderr_bytes = 0u64;
        while !flag.load(Ordering::SeqCst) {
            let Ok((channel, payload)) = read_channel(&mut stream) else {
                break;
            };
            match channel {
                CHANNEL_STDOUT => {
                    if writer.write_all(&payload).is_err() {
                        break;
                    }
                }
                CHANNEL_STDERR => {
                    if stderr_bytes >= MAX_STDERR_BYTES {
                        continue;
                    }
                    stderr_bytes += payload.len() as u64;
                    if let Some(path) = stderr_log.as_ref() {
                        if let Ok(mut file) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(path)
                        {
                            let _ = file.write_all(&payload);
                        }
                    }
                }
                _ => break,
            }
        }
        let _ = writer.shutdown(std::net::Shutdown::Both);
    });
    Ok(OwnerConsole {
        stdin: Some(console),
        stdout: Some(stdout_read),
        reader: Some(reader),
        writer: closed,
        pid,
        detached: false,
    })
}

/// Owner-side directory for one runtime's console socket.
pub(crate) fn socket_path(run_dir: &Path, runtime_id: &str) -> PathBuf {
    run_dir
        .join("runtimes")
        .join(runtime_id)
        .join("console.sock")
}

/// Refuse a path that is not the exact owner-derived console path for this
/// runtime (defence in depth around `socket_path`).
pub(crate) fn validate_path(run_dir: &Path, runtime_id: &str, path: &Path) -> Result<()> {
    let expected = socket_path(run_dir, runtime_id);
    if expected != path || path.parent() != expected.parent() {
        return Err(Error::internal(
            "console path escaped the owned run directory",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_frames_round_trip_and_bound() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        write_channel(&mut a, CHANNEL_STDOUT, b"hello").unwrap();
        write_channel(&mut a, CHANNEL_STDERR, b"warn").unwrap();
        assert_eq!(
            read_channel(&mut b).unwrap(),
            (CHANNEL_STDOUT, b"hello".to_vec())
        );
        assert_eq!(
            read_channel(&mut b).unwrap(),
            (CHANNEL_STDERR, b"warn".to_vec())
        );
        assert!(write_channel(&mut a, 3, b"x").is_err());
        assert!(
            write_channel(&mut a, CHANNEL_STDOUT, &vec![0u8; MAX_CONSOLE_PAYLOAD + 1]).is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn docker_attach_stdout_frame_uses_the_real_header_layout() {
        let (mut docker_writer, mut docker_reader) = UnixStream::pair().unwrap();
        let (mut console_writer, mut console_reader) = UnixStream::pair().unwrap();
        // Exact Docker 29 hijacked-frame bytes for stdout "ok".
        docker_writer
            .write_all(&[1, 0, 0, 0, 0, 0, 0, 2, b'o', b'k'])
            .unwrap();
        docker_writer.shutdown(std::net::Shutdown::Write).unwrap();
        pump_docker(&mut docker_reader, &mut console_writer).unwrap();
        assert_eq!(
            read_channel(&mut console_reader).unwrap(),
            (CHANNEL_STDOUT, b"ok".to_vec())
        );
    }

    #[test]
    fn console_path_is_derived_from_the_run_dir() {
        let run = Path::new("/run/cadence-runtime-owner");
        let path = socket_path(run, "rt-1");
        validate_path(run, "rt-1", &path).unwrap();
        assert!(validate_path(run, "rt-2", &path).is_err());
        assert!(validate_path(run, "../escape", &path).is_err());
    }
}
