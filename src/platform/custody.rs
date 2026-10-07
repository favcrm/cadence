//! ADR 0006 §5.3: where credential bytes live. The agent-visible
//! artifacts are handles and fingerprints, never bytes — custody is
//! the only component that ever holds them.
//!
//! Backends: the OS keychain where the host has one the daemon can use
//! without leaking the bytes (libsecret's `secret-tool` reads the
//! secret on stdin — macOS `security` cannot take one off argv, so it
//! is not a backend), else the daemon-owned `0600` file store.
//!
//! Honest scope of isolation: custody keeps the bytes out of other
//! uids and out of the confined master's read set — and nothing more.
//! The P4 residual stands: a same-uid process that ignores the API
//! reads a custody file or the login keyring, and today only the
//! master runs under Landlock — every managed pty agent is same-uid
//! and unconfined. Until a backend/mode protects custody from them,
//! `platform enroll` refuses (`custody_unprotected`) unless the
//! operator passes `accept_same_uid_risk`; that acceptance lands on
//! the `platform_connected` audit event.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The file store: `<state dir>/custody/<sha256(platform\naccount)>.
/// cred`. Hashed names keep an account handle out of the listing and
/// give rotate a stable name to rename onto.
pub const FILE_TAG: &str = "file";
/// libsecret (GNOME keyring / KeePassXC / …) via `secret-tool`.
pub const LIBSECRET_TAG: &str = "keychain:libsecret";

/// The custody backend this daemon uses — picked once at
/// [`Custody::open`]; every record names the tag it was written under
/// so a load always reaches the backend that holds it.
pub enum Custody {
    /// The `0600` file store at `Custody::File(dir)`.
    File(PathBuf),
    /// `secret-tool` at the resolved path.
    Libsecret(PathBuf),
}

/// `platform`'s credential for `account` — the custody lookup key.
pub struct Key<'a> {
    pub platform: &'a str,
    pub account: &'a str,
}

impl Custody {
    /// Pick the backend this host supports: the keychain when one is
    /// usable, else the file store (§5.3's default).
    pub fn open(state_dir: &Path) -> Result<Custody> {
        if let Some(tool) = libsecret_tool() {
            return Ok(Custody::Libsecret(tool));
        }
        Ok(Custody::File(custody_dir(state_dir)))
    }

    /// The tag written into the record's `custody` column.
    pub fn tag(&self) -> &'static str {
        match self {
            Custody::File(_) => FILE_TAG,
            Custody::Libsecret(_) => LIBSECRET_TAG,
        }
    }

    /// Store `bytes` under `key`. Atomic for the file backend (temp +
    /// rename), so a rotate's overwrite never truncates the old
    /// credential for a concurrent reader.
    pub fn put(&self, key: &Key, bytes: &[u8]) -> Result<&'static str> {
        match self {
            Custody::File(dir) => {
                file_put(dir, key, bytes)?;
                Ok(FILE_TAG)
            }
            Custody::Libsecret(tool) => {
                libsecret_store(tool, key, bytes)?;
                Ok(LIBSECRET_TAG)
            }
        }
    }

    /// The bytes `tag` custody holds for `key`. `tag` comes from the
    /// record — a record enrolled on a keychain this host no longer
    /// has fails here, never silently reads the file store.
    pub fn load(&self, tag: &str, key: &Key) -> Result<Vec<u8>> {
        match (tag, self) {
            (FILE_TAG, Custody::File(dir)) => file_load(dir, key),
            (FILE_TAG, _) => Err(Error::internal(
                "custody record names the file store but this daemon picked the keychain",
            )),
            (LIBSECRET_TAG, Custody::Libsecret(tool)) => libsecret_lookup(tool, key),
            (LIBSECRET_TAG, _) => Err(Error::rejected(format!(
                "credential for '{}/{}' lives in a keychain this host no longer offers",
                key.platform, key.account
            ))),
            (other, _) => Err(Error::internal(format!("unknown custody tag '{other}'"))),
        }
    }

    /// CAD-1065: the `connection_test` read. Same backend selection as
    /// [`load`], but bounded: no read may exceed `cap` bytes and the
    /// whole operation answers inside `deadline` — the file backend
    /// reads incrementally and never allocates past the cap, and a
    /// libsecret lookup drains the owned child's pipes under a poll
    /// deadline, killing and reaping only that child on timeout.
    ///
    /// `fenced` is set when a timed-out child could not be provably
    /// killed and reaped: the verification path stays fenced (every
    /// later call refuses) rather than leave an unobserved process
    /// holding credential bytes.
    pub fn load_bounded(
        &self,
        tag: &str,
        key: &Key,
        cap: usize,
        deadline: &super::OpDeadline,
        fenced: &std::sync::atomic::AtomicBool,
    ) -> Result<Vec<u8>> {
        match (tag, self) {
            (FILE_TAG, Custody::File(dir)) => file_load_bounded(dir, key, cap, deadline),
            (FILE_TAG, _) => Err(Error::internal(
                "custody record names the file store but this daemon picked the keychain",
            )),
            (LIBSECRET_TAG, Custody::Libsecret(tool)) => {
                libsecret_lookup_bounded(tool, key, cap, deadline, fenced)
            }
            (LIBSECRET_TAG, _) => Err(Error::rejected(format!(
                "credential for '{}/{}' lives in a keychain this host no longer offers",
                key.platform, key.account
            ))),
            (other, _) => Err(Error::internal(format!("unknown custody tag '{other}'"))),
        }
    }

    /// Drop `key`'s bytes from the `tag` backend. Idempotent — a
    /// retry after a crash between the custody delete and the record
    /// delete finds nothing and proceeds.
    pub fn remove(&self, tag: &str, key: &Key) -> Result<()> {
        match (tag, self) {
            (FILE_TAG, Custody::File(dir)) => file_remove(dir, key),
            (FILE_TAG, _) => Ok(()),
            (LIBSECRET_TAG, Custody::Libsecret(tool)) => libsecret_clear(tool, key),
            (LIBSECRET_TAG, _) => Err(Error::rejected(format!(
                "credential for '{}/{}' lives in a keychain this host no longer offers — \
                 the record stays until its bytes can be removed",
                key.platform, key.account
            ))),
            (other, _) => Err(Error::internal(format!("unknown custody tag '{other}'"))),
        }
    }
}

/// `<state dir>/custody` — daemon-owned `0700`, isolated from other
/// uids and the confined master's Landlock read set. Unconfined
/// same-uid workers can still read it until CAD-461 separates uids.
pub fn custody_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("custody")
}

fn file_path(dir: &Path, key: &Key) -> PathBuf {
    let name: String = Sha256::digest(format!("{}\n{}", key.platform, key.account).as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    dir.join(format!("{name}.cred"))
}

fn file_put(dir: &Path, key: &Key, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, PermissionsExt::from_mode(0o700))?;
    // Unique per call — concurrent enrolls share the daemon process,
    // never this name.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = dir.join(format!(".{}-{nanos}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| Error::internal(format!("custody tmp {}: {e}", tmp.display())))?;
    // Only clean up after create_new succeeded: a collision belongs
    // to another writer, never this call. Every subsequent error must
    // discard this call's temporary credential bytes.
    let result = (|| -> Result<()> {
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        // `create_new` gives 0600; the set also pins a lingering umask.
        std::fs::set_permissions(&tmp, PermissionsExt::from_mode(0o600))?;
        std::fs::rename(&tmp, file_path(dir, key))?;
        Ok(())
    })();
    if result.is_err() {
        std::fs::remove_file(&tmp)?;
    }
    result?;
    Ok(())
}

fn file_load(dir: &Path, key: &Key) -> Result<Vec<u8>> {
    let path = file_path(dir, key);
    match std::fs::read(&path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::internal(format!(
            "custody holds no bytes for '{}/{}' — the record outlived its store",
            key.platform, key.account
        ))),
        Err(e) => Err(Error::internal(format!("custody read: {e}"))),
    }
}

/// CAD-1065's bounded file read: open, then pull bytes through a
/// capped `Take` so a pathological or torn blob refuses at `cap`
/// instead of landing whole in memory. The deadline is checked
/// before open and between reads; a spent budget fails the load.
fn file_load_bounded(
    dir: &Path,
    key: &Key,
    cap: usize,
    deadline: &super::OpDeadline,
) -> Result<Vec<u8>> {
    use std::io::Read;
    if deadline.expired() {
        return Err(Error::busy("connection test budget is spent"));
    }
    let path = file_path(dir, key);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::internal(format!(
                "custody holds no bytes for '{}/{}' — the record outlived its store",
                key.platform, key.account
            )))
        }
        Err(e) => return Err(Error::internal(format!("custody read: {e}"))),
    };
    let mut bytes = Vec::new();
    let mut limited = std::io::Read::take(file, cap as u64 + 1);
    loop {
        if deadline.expired() {
            return Err(Error::busy("connection test budget is spent"));
        }
        let mut chunk = [0u8; 4096];
        let read = limited
            .read(&mut chunk)
            .map_err(|e| Error::internal(format!("custody read: {e}")))?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > cap {
            return Err(Error::internal(
                "custody bytes exceed their supported bounds",
            ));
        }
    }
    Ok(bytes)
}

fn file_remove(dir: &Path, key: &Key) -> Result<()> {
    match std::fs::remove_file(file_path(dir, key)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::internal(format!("custody delete: {e}"))),
    }
}

/// `secret-tool` on `PATH` with a session bus it would talk to — a
/// daemon without a bus (systemd service, headless host) falls back
/// to the file store rather than fail at each call.
fn libsecret_tool() -> Option<PathBuf> {
    let tool = crate::master::which("secret-tool", std::env::var("PATH").ok().as_deref())?;
    let bus = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| Path::new(&d).join("bus").exists())
            .unwrap_or(false);
    bus.then_some(tool)
}

fn libsecret_args(key: &Key) -> [String; 5] {
    [
        "service".to_string(),
        "cadence-platform".to_string(),
        "platform".to_string(),
        key.platform.to_string(),
        key.account.to_string(),
    ]
}

/// `secret-tool store` — the secret goes on stdin, never argv.
fn libsecret_store(tool: &Path, key: &Key, bytes: &[u8]) -> Result<()> {
    let mut child = crate::reaper::spawn(
        std::process::Command::new(tool)
            .arg("store")
            .arg(format!(
                "--label=cadence platform {}/{}",
                key.platform, key.account
            ))
            .args(libsecret_args(key))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )?;
    // stdin must close before `wait` — `store` reads to EOF.
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(bytes)
        .and_then(|()| child.wait())
        .map_err(|e| Error::internal(format!("secret-tool store: {e}")))?
        .success()
        .then_some(())
        .ok_or_else(|| Error::internal("secret-tool store failed"))
}

fn libsecret_lookup(tool: &Path, key: &Key) -> Result<Vec<u8>> {
    let out = crate::reaper::output(
        std::process::Command::new(tool)
            .arg("lookup")
            .args(libsecret_args(key)),
    )?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "keychain holds no credential for '{}/{}'",
            key.platform, key.account
        )));
    }
    // `secret-tool` appends a newline to the secret it prints — it is
    // the tool's framing, never part of the credential; leaving it on
    // would fail the record's fingerprint check.
    let mut bytes = out.stdout;
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    Ok(bytes)
}

/// CAD-1065's bounded libsecret lookup. The owned child is spawned
/// through [`crate::reaper::spawn`] (registered, never adopted), its
/// stdin is null and both pipes are drained under `libc::poll` for the
/// operation deadline — never the unbounded `wait_with_output` of
/// [`crate::reaper::output`]. More than `cap` bytes of output refuses;
/// a spent deadline kills and reaps only this child, and a kill or
/// reap that cannot be observed sets `fenced` so this verification
/// path stays closed.
#[cfg(unix)]
fn libsecret_lookup_bounded(
    tool: &Path,
    key: &Key,
    cap: usize,
    deadline: &super::OpDeadline,
    fenced: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;

    let mut child = crate::reaper::spawn(
        std::process::Command::new(tool)
            .arg("lookup")
            .args(libsecret_args(key))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut open = [true, true];
    let result = 'drain: loop {
        let mut fds = [
            libc::pollfd {
                fd: stdout.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stderr.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let remaining = match deadline.remaining() {
            Some(left) => left,
            None => break 'drain Err(Error::busy("connection test budget is spent")),
        };
        let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: fds point at live pollfd entries for this call only.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) };
        if ready < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break 'drain Err(Error::internal(format!("secret-tool lookup: {e}")));
        }
        if ready == 0 {
            break 'drain Err(Error::busy("connection test budget is spent"));
        }
        for i in 0..2usize {
            if !open[i] {
                continue;
            }
            let revents = fds[i].revents;
            if revents == 0 {
                continue;
            }
            let (pipe, acc): (&mut dyn Read, &mut Vec<u8>) = if i == 0 {
                (&mut stdout, &mut out)
            } else {
                (&mut stderr, &mut err)
            };
            if revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                let mut chunk = [0u8; 4096];
                match pipe.read(&mut chunk) {
                    Ok(0) => open[i] = false,
                    Ok(n) => {
                        acc.extend_from_slice(&chunk[..n]);
                        if acc.len() > cap {
                            break 'drain Err(Error::internal(
                                "keychain output exceeds its supported bounds",
                            ));
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        break 'drain Err(Error::internal(format!("secret-tool lookup: {e}")))
                    }
                }
            }
            if revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                open[i] = false;
            }
        }
        if !open[0] && !open[1] {
            break 'drain match child.wait() {
                Ok(status) if status.success() => Ok(out),
                Ok(_) => Err(Error::internal(format!(
                    "keychain holds no credential for '{}/{}'",
                    key.platform, key.account
                ))),
                Err(e) => Err(Error::internal(format!("secret-tool lookup: {e}"))),
            };
        }
    };
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(error) => {
            // The child is dead or unreachable inside the deadline: kill
            // and reap only this owned process. A kill or reap whose
            // outcome cannot be observed leaves a process that may still
            // hold the credential bytes — the verification path fences.
            let killed = child.kill().is_ok() || child.try_wait().ok().flatten().is_some();
            let reaped = killed && child.wait().is_ok();
            if !reaped {
                fenced.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            return Err(error);
        }
    };
    // `secret-tool` appends a newline to the secret it prints — it is
    // the tool's framing, never part of the credential.
    let mut bytes = bytes;
    while matches!(bytes.last(), Some(b'\n' | b'\r')) {
        bytes.pop();
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn libsecret_lookup_bounded(
    _tool: &Path,
    _key: &Key,
    _cap: usize,
    _deadline: &super::OpDeadline,
    _fenced: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>> {
    Err(Error::internal("the bounded keychain lookup is unix-only"))
}

fn libsecret_clear(tool: &Path, key: &Key) -> Result<()> {
    let out = crate::reaper::output(
        std::process::Command::new(tool)
            .arg("clear")
            .args(libsecret_args(key)),
    )?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "keychain refused to drop '{}/{}'",
            key.platform, key.account
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn failed_file_put_cleans_only_its_own_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        // A directory at the final credential path makes rename fail
        // after the temporary file was created and written.
        std::fs::create_dir(file_path(dir.path(), &key())).unwrap();
        let foreign = dir.path().join(".another-writer.tmp");
        std::fs::write(&foreign, b"another write").unwrap();
        std::thread::scope(|scope| {
            let writers: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| file_put(dir.path(), &key(), b"synthetic bytes")))
                .collect();
            for writer in writers {
                assert!(writer.join().unwrap().is_err());
            }
        });
        let temporaries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
            .collect();
        assert_eq!(temporaries, vec![foreign]);
    }

    /// A stand-in `secret-tool`: `lookup` prints the secret with the
    /// trailing newline the real tool adds; `clear` exits 0.
    fn stub_tool(dir: &Path, body: &str) -> PathBuf {
        let tool = dir.join("secret-tool");
        let mut f = std::fs::File::create(&tool).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().unwrap();
        std::fs::set_permissions(&tool, PermissionsExt::from_mode(0o755)).unwrap();
        tool
    }

    fn key() -> Key<'static> {
        Key {
            platform: "github",
            account: "acme",
        }
    }

    /// A lookup's bytes are exactly the stored secret — the newline
    /// `secret-tool` prints after it is framing, not credential: the
    /// recorded fingerprint must still match.
    #[test]
    fn libsecret_lookup_trims_the_tool_framing() {
        let dir = tempfile::tempdir().unwrap();
        let tool = stub_tool(dir.path(), "#!/bin/sh\nprintf 'tok_abc123\\n'\n");
        let bytes = libsecret_lookup(&tool, &key()).unwrap();
        assert_eq!(bytes, b"tok_abc123");
    }

    /// CRLF framing and a secret that legitimately ends in a newline
    /// indistinguishable boundary: a lone stored `\n` is still the
    /// tool's — the recorded secret was written without one.
    #[test]
    fn libsecret_lookup_trims_crlf_too() {
        let dir = tempfile::tempdir().unwrap();
        let tool = stub_tool(dir.path(), "#!/bin/sh\nprintf 'tok_abc123\\r\\n'\n");
        let bytes = libsecret_lookup(&tool, &key()).unwrap();
        assert_eq!(bytes, b"tok_abc123");
    }

    /// A failing tool surfaces as an error, never empty bytes.
    #[test]
    fn libsecret_lookup_refuses_a_failed_tool() {
        let dir = tempfile::tempdir().unwrap();
        let tool = stub_tool(dir.path(), "#!/bin/sh\nexit 1\n");
        assert!(libsecret_lookup(&tool, &key()).is_err());
    }
}
