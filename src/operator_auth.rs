//! Operator identity for the web UI (CAD-313, ADR 0004 phase 1): the
//! operator secret, single-use login links and board sessions. The
//! daemon owns all three ([`Auth`] lives in its process); the board and
//! the CLI only ever ask it.
//!
//! - **The secret** — `<state>/operator/secret`, 32 random bytes as
//!   hex, `0600` in a `0700` directory, both owned by this euid. Every
//!   reader applies ssh-style strict modes ([`read_secret`]) and
//!   refuses, naming the fix, rather than repairing anything. It never
//!   enters argv, the environment, a URL or a log: the CLI reads the
//!   file and hands it to the daemon over the `0700` socket.
//! - **A login link** — minted by `operator_link_mint` for a caller
//!   that passes positive operator proof AND presents the secret. The
//!   nonce is 32 random bytes, valid [`LINK_TTL_SECS`], single use,
//!   bound to one [`Origin`]; only its sha256 is kept. The CLI prints it
//!   in the URL fragment, which no browser sends to a server.
//! - **A session** — what exchanging a link yields: a 32-byte token the
//!   board sets as an HttpOnly cookie. Only `sha256(token)` is stored
//!   (`<state>/operator/sessions.json`, `0600`), bound to the link's
//!   origin, with an idle ([`IDLE_SECS`]) and an absolute
//!   ([`ABSOLUTE_SECS`]) expiry.
//!
//! What the secret does not do (ADR 0004 §5.1): it is no boundary
//! against a process of the operator's own uid, which can read the file.
//! Phase 1 makes impersonation deliberate — read the secret AND evade
//! [`crate::peer::operator_proof`] — instead of the default.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// The directory under the state dir that holds the secret and the
/// session hashes. Backups never read it (CAD-314 copies the store only).
pub const DIR: &str = "operator";
const SECRET: &str = "secret";
const SESSIONS: &str = "sessions.json";

/// A login link's lifetime.
pub const LINK_TTL_SECS: i64 = 120;
/// A session unused this long is gone.
pub const IDLE_SECS: i64 = 24 * 3600;
/// No session outlives this, however much it is used.
pub const ABSOLUTE_SECS: i64 = 7 * 24 * 3600;
/// `last_used` is written at most this often — a busy tab does not
/// rewrite the file on every request.
const TOUCH_EVERY_SECS: i64 = 60;
/// A secret, nonce or token: 32 bytes as lowercase hex.
const CREDENTIAL_HEX: usize = 64;

/// Where a login link may be exchanged and a session used: the board on
/// this host (any loopback Host) or through the proven `tailscale serve`
/// proxy (CAD-336). A session never crosses from one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Loopback,
    Tailnet,
}

impl Origin {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "loopback" => Some(Self::Loopback),
            "tailnet" => Some(Self::Tailnet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Tailnet => "tailnet",
        }
    }
}

// ---------- credentials ----------

/// 32 bytes from the kernel's CSPRNG (`getrandom(2)`), as hex.
pub fn random_credential() -> Result<String> {
    let mut buf = [0u8; 32];
    let mut filled = 0;
    while filled < buf.len() {
        // SAFETY: the pointer and length name the unfilled tail of `buf`.
        let n =
            unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(Error::internal(format!("getrandom: {e}")));
        }
        filled += n as usize;
    }
    Ok(hex(&buf))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `sha256(credential)` as hex — the only form a nonce or token is kept in.
pub fn digest(credential: &str) -> String {
    hex(&Sha256::digest(credential.as_bytes()))
}

/// Does `presented` equal `expected`? Compared as sha256 digests, every
/// byte visited, so the time taken says nothing about where they differ.
pub fn same_credential(presented: &str, expected: &str) -> bool {
    let a = Sha256::digest(presented.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// 64 lowercase hex characters — the only shape a credential has. Anything
/// else is refused before it is hashed or looked up.
pub fn well_formed(credential: &str) -> bool {
    credential.len() == CREDENTIAL_HEX
        && credential
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---------- the secret file ----------

fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

fn refuse(message: String) -> Error {
    Error::invalid("operator_secret", message)
}

pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join(DIR)
}

pub fn secret_path(state_dir: &Path) -> PathBuf {
    dir(state_dir).join(SECRET)
}

/// The directory must be a real directory (never a symlink), owned by
/// this euid, with no group or other bit.
fn check_dir(dir: &Path) -> Result<()> {
    let md = fs::symlink_metadata(dir).map_err(|e| {
        refuse(format!(
            "{}: {e} — run `cadence ui login` to create the operator secret",
            dir.display()
        ))
    })?;
    if !md.is_dir() {
        return Err(refuse(format!(
            "{} is not a directory (a symlink or a file) — refusing it; move it aside \
             and run `cadence ui login --rotate`",
            dir.display()
        )));
    }
    if md.uid() != euid() {
        return Err(refuse(format!(
            "{} is owned by uid {}, not this user (uid {}) — refusing it",
            dir.display(),
            md.uid(),
            euid()
        )));
    }
    if md.mode() & 0o077 != 0 {
        return Err(refuse(format!(
            "{} has mode {:03o}; it must be private — run `chmod 700 {}`",
            dir.display(),
            md.mode() & 0o777,
            dir.display()
        )));
    }
    Ok(())
}

/// Create the secret when there is none (`O_CREAT|O_EXCL|O_NOFOLLOW`,
/// mode `0600`, in a `0700` directory). An existing file is left
/// exactly as it is — a wrong mode is [`read_secret`]'s refusal, never
/// silently fixed. `true` when this call created it.
pub fn ensure_secret(state_dir: &Path) -> Result<bool> {
    let dir = dir(state_dir);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(Error::internal(format!("{}: {e}", dir.display()))),
    }
    check_dir(&dir)?;
    let path = dir.join(SECRET);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path);
    let mut file = match file {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(e) => return Err(Error::internal(format!("{}: {e}", path.display()))),
    };
    file.write_all(format!("{}\n", random_credential()?).as_bytes())?;
    file.sync_all()?;
    Ok(true)
}

/// Read the secret under strict modes, refusing (never repairing) when
/// the directory fails [`check_dir`], the file is a symlink or not a
/// regular file, is not owned by this euid, has any group or other bit,
/// or does not hold one well-formed credential.
pub fn read_secret(state_dir: &Path) -> Result<String> {
    let dir = dir(state_dir);
    check_dir(&dir)?;
    let path = dir.join(SECRET);
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                refuse(format!(
                    "{} is a symlink — refusing it; remove it and run \
                     `cadence ui login --rotate`",
                    path.display()
                ))
            } else {
                refuse(format!(
                    "{}: {e} — run `cadence ui login` to create it",
                    path.display()
                ))
            }
        })?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(refuse(format!(
            "{} is not a regular file — refusing it",
            path.display()
        )));
    }
    if md.uid() != euid() {
        return Err(refuse(format!(
            "{} is owned by uid {}, not this user (uid {}) — refusing it",
            path.display(),
            md.uid(),
            euid()
        )));
    }
    if md.mode() & 0o077 != 0 {
        return Err(refuse(format!(
            "{} has mode {:03o}; it must be readable by this user only — run \
             `chmod 600 {}`, then `cadence ui login --rotate` (another user may have read it)",
            path.display(),
            md.mode() & 0o777,
            path.display()
        )));
    }
    let mut raw = String::new();
    (&mut file).take(256).read_to_string(&mut raw)?;
    let secret = raw.trim();
    if !well_formed(secret) {
        return Err(refuse(format!(
            "{} does not hold a well-formed secret — run `cadence ui login --rotate`",
            path.display()
        )));
    }
    Ok(secret.to_string())
}

/// Replace the secret with a fresh one: a new `0600` file, then an
/// atomic rename over the old. The directory's strict modes still apply.
pub fn rotate_secret(state_dir: &Path) -> Result<()> {
    let dir = dir(state_dir);
    check_dir(&dir)?;
    let tmp = dir.join("secret.tmp");
    match fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Error::internal(format!("{}: {e}", tmp.display()))),
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)?;
    file.write_all(format!("{}\n", random_credential()?).as_bytes())?;
    file.sync_all()?;
    fs::rename(&tmp, dir.join(SECRET))?;
    Ok(())
}

// ---------- links and sessions ----------

/// Why a link exchange was refused. Each is loud: the browser shows it,
/// and the daemon records `operator_link_rejected` with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkRefusal {
    /// Not a credential's shape.
    Malformed,
    /// Never minted here, or the daemon restarted since.
    Unknown,
    /// Exchanged before — someone else may have opened it first.
    AlreadyUsed,
    /// Older than [`LINK_TTL_SECS`].
    Expired,
    /// Minted for the other origin (loopback vs tailnet).
    WrongOrigin,
}

impl LinkRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Unknown => "unknown",
            Self::AlreadyUsed => "already_used",
            Self::Expired => "expired",
            Self::WrongOrigin => "wrong_origin",
        }
    }

    pub fn explain(self) -> &'static str {
        match self {
            Self::Malformed => "this is not a login link",
            Self::Unknown => {
                "this login link is unknown here — it was never minted by this daemon, \
                 or the daemon restarted; run `cadence ui login` again"
            }
            Self::AlreadyUsed => {
                "this login link was already used — if that was not you, run \
                 `cadence ui sessions --revoke-all`"
            }
            Self::Expired => "this login link expired — run `cadence ui login` again",
            Self::WrongOrigin => {
                "this login link was minted for another address (loopback vs tailnet) — \
                 run `cadence ui login` (or `--tailnet`) for the address you opened"
            }
        }
    }
}

/// One stored session: the token's hash and what `ui sessions` shows.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    hash: String,
    origin: Origin,
    created: i64,
    last_used: i64,
    expires_at: i64,
    #[serde(default)]
    user_agent: String,
}

impl Row {
    /// The display id: the first 8 hex digits of the hash — enough to
    /// revoke one, useless as a credential.
    fn id(&self) -> &str {
        &self.hash[..8]
    }

    fn live(&self, now: i64) -> bool {
        now < self.expires_at && now - self.last_used < IDLE_SECS
    }

    fn view(&self) -> SessionView {
        SessionView {
            id: self.id().to_string(),
            origin: self.origin,
            created: self.created,
            last_used: self.last_used,
            idle_expires_at: (self.last_used + IDLE_SECS).min(self.expires_at),
            expires_at: self.expires_at,
            user_agent: self.user_agent.clone(),
        }
    }
}

/// A session as the board and `ui sessions` see it — never the token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionView {
    pub id: String,
    pub origin: Origin,
    pub created: i64,
    pub last_used: i64,
    pub idle_expires_at: i64,
    pub expires_at: i64,
    pub user_agent: String,
}

/// A freshly opened session: the token goes to the board once, for its
/// `Set-Cookie`, and is never stored or returned again.
pub struct Opened {
    pub token: String,
    pub session: SessionView,
}

#[derive(Serialize, Deserialize, Default)]
struct SessionsFile {
    sessions: Vec<Row>,
}

/// The daemon's operator-auth state: live link nonces (memory only — a
/// restart voids every unexchanged link), the spent ones (so a replay
/// says "already used", not "unknown"), and the persisted sessions.
///
/// There is deliberately no rate limit on failed exchanges: a nonce is
/// 256 random bits, so guessing gains nothing, while any shared failure
/// budget is one an agent can spend with no credential at all to lock
/// the operator's fresh link out (review of PR #249).
pub struct Auth {
    path: PathBuf,
    sessions: Vec<Row>,
    /// nonce hash → (origin, expires_at)
    links: HashMap<String, (Origin, i64)>,
    /// spent nonce hash → when it would have expired (pruned after)
    spent: HashMap<String, i64>,
}

/// Printable ASCII, bounded — the user agent is shown by `ui sessions`.
fn clean_user_agent(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .take(160)
        .collect()
}

impl Auth {
    /// Load the persisted sessions of `state_dir`. A missing file is no
    /// sessions; an unreadable one is also none — logging everyone out
    /// is the safe failure.
    pub fn load(state_dir: &Path) -> Self {
        let path = dir(state_dir).join(SESSIONS);
        let sessions = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SessionsFile>(&b).ok())
            .map(|f| f.sessions)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| well_formed(&r.hash))
            .collect();
        Self {
            path,
            sessions,
            links: HashMap::new(),
            spent: HashMap::new(),
        }
    }

    fn persist(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .ok_or_else(|| Error::internal("sessions file has no directory"))?;
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(Error::internal(format!("{}: {e}", dir.display()))),
        }
        check_dir(dir)?;
        let tmp = self.path.with_extension("tmp");
        match fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::internal(format!("{}: {e}", tmp.display()))),
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        let body = SessionsFile {
            sessions: self.sessions.clone(),
        };
        file.write_all(&serde_json::to_vec_pretty(&body)?)?;
        file.sync_all()?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    fn prune(&mut self, now: i64) -> bool {
        // An expired link is kept one more TTL so its refusal can say
        // "expired" rather than "unknown".
        self.links.retain(|_, (_, exp)| now <= *exp + LINK_TTL_SECS);
        self.spent.retain(|_, exp| now <= *exp + LINK_TTL_SECS);
        let before = self.sessions.len();
        self.sessions.retain(|r| r.live(now));
        before != self.sessions.len()
    }

    /// Mint a single-use link nonce for `origin`. The caller has already
    /// proved the operator; this only records `sha256(nonce)`.
    pub fn mint(&mut self, origin: Origin, now: i64) -> Result<String> {
        self.prune(now);
        let nonce = random_credential()?;
        self.links
            .insert(digest(&nonce), (origin, now + LINK_TTL_SECS));
        Ok(nonce)
    }

    /// Exchange `nonce` for a session on `origin`. The nonce is spent by
    /// the first attempt that names it, whatever the outcome — a refused
    /// exchange (wrong origin, too late) cannot be retried.
    pub fn open(
        &mut self,
        nonce: &str,
        origin: Origin,
        user_agent: &str,
        now: i64,
    ) -> std::result::Result<Result<Opened>, LinkRefusal> {
        self.prune(now);
        if !well_formed(nonce) {
            return Err(LinkRefusal::Malformed);
        }
        let hash = digest(nonce);
        if self.spent.contains_key(&hash) {
            return Err(LinkRefusal::AlreadyUsed);
        }
        let Some((minted_for, expires)) = self.links.remove(&hash) else {
            return Err(LinkRefusal::Unknown);
        };
        self.spent.insert(hash, expires);
        if now > expires {
            return Err(LinkRefusal::Expired);
        }
        if minted_for != origin {
            return Err(LinkRefusal::WrongOrigin);
        }
        Ok(self.create(origin, user_agent, now))
    }

    fn create(&mut self, origin: Origin, user_agent: &str, now: i64) -> Result<Opened> {
        let token = random_credential()?;
        let row = Row {
            hash: digest(&token),
            origin,
            created: now,
            last_used: now,
            expires_at: now + ABSOLUTE_SECS,
            user_agent: clean_user_agent(user_agent),
        };
        let session = row.view();
        self.sessions.push(row);
        self.persist()?;
        Ok(Opened { token, session })
    }

    /// The live session `token` names on `origin`, touching its idle
    /// clock. A token presented on the other origin is no session there.
    pub fn check(&mut self, token: &str, origin: Origin, now: i64) -> Result<Option<SessionView>> {
        let mut dirty = self.prune(now);
        if !well_formed(token) {
            if dirty {
                self.persist()?;
            }
            return Ok(None);
        }
        let hash = digest(token);
        let found = self
            .sessions
            .iter_mut()
            .find(|r| same_credential(&r.hash, &hash) && r.origin == origin)
            .map(|row| {
                if now - row.last_used >= TOUCH_EVERY_SECS {
                    row.last_used = now;
                    dirty = true;
                }
                row.view()
            });
        if dirty {
            self.persist()?;
        }
        Ok(found)
    }

    /// End the session `token` names, on any origin. `true` if it existed.
    pub fn revoke_token(&mut self, token: &str) -> Result<bool> {
        if !well_formed(token) {
            return Ok(false);
        }
        let hash = digest(token);
        let before = self.sessions.len();
        self.sessions.retain(|r| r.hash != hash);
        let gone = before != self.sessions.len();
        if gone {
            self.persist()?;
        }
        Ok(gone)
    }

    /// End every session whose display id is `id`.
    pub fn revoke_id(&mut self, id: &str) -> Result<usize> {
        let before = self.sessions.len();
        self.sessions.retain(|r| r.id() != id);
        let gone = before - self.sessions.len();
        if gone > 0 {
            self.persist()?;
        }
        Ok(gone)
    }

    /// End every session and void every unexchanged link.
    pub fn revoke_all(&mut self) -> Result<usize> {
        let gone = self.sessions.len();
        self.sessions.clear();
        for (hash, (_, exp)) in self.links.drain() {
            self.spent.insert(hash, exp);
        }
        self.persist()?;
        Ok(gone)
    }

    /// The live sessions, oldest first.
    pub fn list(&mut self, now: i64) -> Result<Vec<SessionView>> {
        if self.prune(now) {
            self.persist()?;
        }
        Ok(self.sessions.iter().map(Row::view).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn state() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().mode() & 0o777
    }

    #[test]
    fn credentials_are_random_hex_and_compare_by_value() {
        let a = random_credential().unwrap();
        let b = random_credential().unwrap();
        assert!(well_formed(&a) && well_formed(&b));
        assert_ne!(a, b);
        assert!(same_credential(&a, &a.clone()));
        assert!(!same_credential(&a, &b));
        assert!(!well_formed(&a.to_uppercase()));
        assert!(!well_formed(&a[..63]));
        assert!(!well_formed(""));
    }

    /// S1: created `0600` in a `0700` directory; a second ensure leaves
    /// it untouched.
    #[test]
    fn secret_is_created_private_once() {
        let s = state();
        assert!(ensure_secret(s.path()).unwrap());
        assert_eq!(mode(&dir(s.path())), 0o700);
        assert_eq!(mode(&secret_path(s.path())), 0o600);
        let first = read_secret(s.path()).unwrap();
        assert!(!ensure_secret(s.path()).unwrap());
        assert_eq!(read_secret(s.path()).unwrap(), first);
    }

    /// S2: every loose shape is refused and never repaired — the mode is
    /// exactly what it was after the refusal.
    #[test]
    fn a_loose_secret_is_refused_never_repaired() {
        for loose in [0o640, 0o604, 0o620, 0o602, 0o644, 0o660] {
            let s = state();
            ensure_secret(s.path()).unwrap();
            let path = secret_path(s.path());
            chmod(&path, loose);
            let err = read_secret(s.path()).unwrap_err();
            assert_eq!(err.code(), Some("operator_secret"), "{loose:o}: {err}");
            assert!(err.to_string().contains("chmod 600"), "{err}");
            assert_eq!(mode(&path), loose, "the refusal must not repair");
        }
        // The directory, too.
        for loose in [0o755, 0o750, 0o705, 0o770] {
            let s = state();
            ensure_secret(s.path()).unwrap();
            chmod(&dir(s.path()), loose);
            let err = read_secret(s.path()).unwrap_err();
            assert!(err.to_string().contains("chmod 700"), "{loose:o}: {err}");
            assert_eq!(mode(&dir(s.path())), loose);
            // ensure refuses it the same way and creates nothing.
            assert!(ensure_secret(s.path()).is_err());
        }
    }

    #[test]
    fn a_symlinked_secret_or_directory_is_refused() {
        let s = state();
        ensure_secret(s.path()).unwrap();
        let real = s.path().join("elsewhere");
        fs::write(&real, format!("{}\n", random_credential().unwrap())).unwrap();
        chmod(&real, 0o600);
        let path = secret_path(s.path());
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        let err = read_secret(s.path()).unwrap_err();
        assert!(err.to_string().contains("symlink"), "{err}");

        let s = state();
        let target = s.path().join("target");
        fs::DirBuilder::new().mode(0o700).create(&target).unwrap();
        std::os::unix::fs::symlink(&target, dir(s.path())).unwrap();
        let err = read_secret(s.path()).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn a_malformed_secret_is_refused() {
        let s = state();
        ensure_secret(s.path()).unwrap();
        fs::write(secret_path(s.path()), "short\n").unwrap();
        let err = read_secret(s.path()).unwrap_err();
        assert!(err.to_string().contains("well-formed"), "{err}");
    }

    #[test]
    fn rotation_replaces_the_secret_privately() {
        let s = state();
        ensure_secret(s.path()).unwrap();
        let before = read_secret(s.path()).unwrap();
        rotate_secret(s.path()).unwrap();
        let after = read_secret(s.path()).unwrap();
        assert_ne!(before, after);
        assert_eq!(mode(&secret_path(s.path())), 0o600);
    }

    const T0: i64 = 1_800_000_000;

    fn opened(auth: &mut Auth, origin: Origin, now: i64) -> Opened {
        let nonce = auth.mint(origin, now).unwrap();
        auth.open(&nonce, origin, "ua", now).unwrap().unwrap()
    }

    /// L1: a nonce opens one session; the replay is `already_used`.
    #[test]
    fn a_link_is_single_use() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let nonce = auth.mint(Origin::Loopback, T0).unwrap();
        assert!(auth.open(&nonce, Origin::Loopback, "ua", T0).is_ok());
        let again = auth.open(&nonce, Origin::Loopback, "ua", T0 + 1);
        assert_eq!(again.err(), Some(LinkRefusal::AlreadyUsed));
    }

    /// L2: expired at 121 s, fine at 120 s.
    #[test]
    fn a_link_expires() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let late = auth.mint(Origin::Loopback, T0).unwrap();
        let err = auth.open(&late, Origin::Loopback, "ua", T0 + LINK_TTL_SECS + 1);
        assert_eq!(err_kind(&err), Some(LinkRefusal::Expired));
        let on_time = auth.mint(Origin::Loopback, T0).unwrap();
        assert!(auth
            .open(&on_time, Origin::Loopback, "ua", T0 + LINK_TTL_SECS)
            .is_ok());
    }

    fn err_kind(r: &std::result::Result<Result<Opened>, LinkRefusal>) -> Option<LinkRefusal> {
        r.as_ref().err().copied()
    }

    /// L3: a link is bound to its origin, and a wrong-origin attempt
    /// spends it.
    #[test]
    fn a_link_is_bound_to_its_origin() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let nonce = auth.mint(Origin::Tailnet, T0).unwrap();
        let err = auth.open(&nonce, Origin::Loopback, "ua", T0);
        assert_eq!(err_kind(&err), Some(LinkRefusal::WrongOrigin));
        let err = auth.open(&nonce, Origin::Tailnet, "ua", T0);
        assert_eq!(err_kind(&err), Some(LinkRefusal::AlreadyUsed));
        let nonce = auth.mint(Origin::Loopback, T0).unwrap();
        let err = auth.open(&nonce, Origin::Tailnet, "ua", T0);
        assert_eq!(err_kind(&err), Some(LinkRefusal::WrongOrigin));
    }

    /// No failure budget: a flood of bogus nonces never locks the
    /// operator's live link out.
    #[test]
    fn bogus_exchanges_never_lock_out_a_live_link() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let live = auth.mint(Origin::Loopback, T0).unwrap();
        for _ in 0..1000 {
            let bogus = random_credential().unwrap();
            let err = auth.open(&bogus, Origin::Loopback, "ua", T0);
            assert_eq!(err_kind(&err), Some(LinkRefusal::Unknown));
        }
        assert!(auth.open(&live, Origin::Loopback, "ua", T0).is_ok());
    }

    /// A session is bound to its origin, idles out and ends absolutely.
    #[test]
    fn sessions_check_origin_idle_and_absolute_expiry() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let o = opened(&mut auth, Origin::Loopback, T0);
        assert!(auth
            .check(&o.token, Origin::Loopback, T0)
            .unwrap()
            .is_some());
        assert!(auth.check(&o.token, Origin::Tailnet, T0).unwrap().is_none());
        // Idle: untouched for 24 h is gone.
        assert!(auth
            .check(&o.token, Origin::Loopback, T0 + IDLE_SECS)
            .unwrap()
            .is_none());
        // Absolute: used every hour, still gone after 7 days.
        let o = opened(&mut auth, Origin::Loopback, T0);
        let mut now = T0;
        while now + 3600 < T0 + ABSOLUTE_SECS {
            now += 3600;
            assert!(auth
                .check(&o.token, Origin::Loopback, now)
                .unwrap()
                .is_some());
        }
        assert!(auth
            .check(&o.token, Origin::Loopback, T0 + ABSOLUTE_SECS)
            .unwrap()
            .is_none());
    }

    /// O3 + L7 at unit level: sessions survive a reload, the file is
    /// `0600` and holds hashes only.
    #[test]
    fn sessions_persist_as_hashes_only() {
        let s = state();
        ensure_secret(s.path()).unwrap();
        let mut auth = Auth::load(s.path());
        let o = opened(&mut auth, Origin::Loopback, T0);
        let file = dir(s.path()).join(SESSIONS);
        assert_eq!(mode(&file), 0o600);
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&o.token), "raw token persisted");
        assert!(text.contains(&digest(&o.token)));
        let mut reloaded = Auth::load(s.path());
        assert_eq!(
            reloaded
                .check(&o.token, Origin::Loopback, T0 + 1)
                .unwrap()
                .map(|v| v.id),
            Some(o.session.id)
        );
    }

    /// L6: revoke one, revoke by id, revoke all (which voids links too).
    #[test]
    fn revocation_invalidates_the_right_set() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let a = opened(&mut auth, Origin::Loopback, T0);
        let b = opened(&mut auth, Origin::Loopback, T0);
        let c = opened(&mut auth, Origin::Tailnet, T0);
        assert!(auth.revoke_token(&a.token).unwrap());
        assert!(auth
            .check(&a.token, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&b.token, Origin::Loopback, T0)
            .unwrap()
            .is_some());
        assert_eq!(auth.revoke_id(&b.session.id).unwrap(), 1);
        assert!(auth
            .check(&b.token, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth.check(&c.token, Origin::Tailnet, T0).unwrap().is_some());
        let pending = auth.mint(Origin::Loopback, T0).unwrap();
        assert_eq!(auth.revoke_all().unwrap(), 1);
        assert!(auth.check(&c.token, Origin::Tailnet, T0).unwrap().is_none());
        let err = auth.open(&pending, Origin::Loopback, "ua", T0);
        assert_eq!(err_kind(&err), Some(LinkRefusal::AlreadyUsed));
    }
}
