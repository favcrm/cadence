//! Operator identity for the web UI (CAD-313, ADR 0004 phase 1): the
//! operator secret, single-use login links and board sessions. The
//! daemon owns all three (its auth state lives in its process); the board and
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

/// A login link's lifetime.
pub const LINK_TTL_SECS: i64 = 120;
/// A session unused this long is gone.
pub const IDLE_SECS: i64 = 24 * 3600;
/// No session outlives this, however much it is used.
pub const ABSOLUTE_SECS: i64 = 7 * 24 * 3600;
/// A secret, nonce or token: 32 bytes as lowercase hex.
const CREDENTIAL_HEX: usize = 64;
/// A public (CAD-526) session's absolute lifetime — the contract's 60
/// minutes. The cookie's `Max-Age` matches; there is no idle extension.
pub const PUBLIC_SESSION_SECS: i64 = 60 * 60;

/// Where a login link may be exchanged and a session used: the board on
/// this host (any loopback Host), through the proven `tailscale serve`
/// proxy (CAD-336), or on the board's configured public name — the
/// AgenticOS-asserted surface (CAD-526). A session never crosses from
/// one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Loopback,
    Tailnet,
    Public,
}

impl Origin {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "loopback" => Some(Self::Loopback),
            "tailnet" => Some(Self::Tailnet),
            "public" => Some(Self::Public),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Tailnet => "tailnet",
            Self::Public => "public",
        }
    }
}

// ---------- credentials ----------

/// 32 bytes from the kernel's CSPRNG, as hex — `getrandom(2)` on
/// Linux, `getentropy(3)` on macOS/*BSD, chosen per target by the
/// `getrandom` crate. `fill` either fills the whole buffer or errors;
/// a credential never ships partially random.
pub fn random_credential() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| Error::internal(format!("getrandom: {e}")))?;
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
pub fn check_dir(dir: &Path) -> Result<()> {
    let md = fs::symlink_metadata(dir).map_err(|e| {
        refuse(format!(
            "Path {}: {e} — run `cadence ui login` to create the operator secret",
            crate::printed_command::shown(dir)
        ))
    })?;
    if !md.is_dir() {
        return Err(refuse(format!(
            "Path {} is not a directory (a symlink or a file) — refusing it; move it aside \
             and run `cadence ui login --rotate`",
            crate::printed_command::shown(dir)
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
            "Path {} has mode {:03o}; it must be private — run {}",
            crate::printed_command::shown(dir),
            md.mode() & 0o777,
            crate::printed_command::with_path("chmod 700", dir, "")
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
                    "Path {} is a symlink — refusing it; remove it and run \
                     `cadence ui login --rotate`",
                    crate::printed_command::shown(&path)
                ))
            } else {
                refuse(format!(
                    "Path {}: {e} — run `cadence ui login` to create it",
                    crate::printed_command::shown(&path)
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
            "Path {} has mode {:03o}; it must be readable by this user only — run \
             {}, then `cadence ui login --rotate` (another user may have read it)",
            crate::printed_command::shown(&path),
            md.mode() & 0o777,
            crate::printed_command::with_path("chmod 600", &path, "")
        )));
    }
    let mut raw = String::new();
    (&mut file).take(256).read_to_string(&mut raw)?;
    let secret = raw.trim();
    if !well_formed(secret) {
        return Err(refuse(format!(
            "Path {} does not hold a well-formed secret — run `cadence ui login --rotate`",
            crate::printed_command::shown(&path)
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

/// Write a small private file under the operator directory — `0600`,
/// tmp+rename so a partial write never replaces a good one. Used for
/// trust-root config the daemon reads back (CAD-526 board identity).
/// The directory is created `0700` when absent and strict-checked
/// always.
pub fn write_private(state_dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let dir = dir(state_dir);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(Error::internal(format!("{}: {e}", dir.display()))),
    }
    check_dir(&dir)?;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') {
        return Err(Error::internal(format!(
            "operator file name '{name}' is not a bare file name"
        )));
    }
    let path = dir.join(name);
    let tmp = dir.join(format!("{name}.tmp"));
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
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Read a private file under the operator directory, under the same
/// strict modes as the secret: real directory owned by this euid with
/// no group/other bit, then a `O_NOFOLLOW` open of a regular file.
/// Missing/unreadable yields `Err` with `capability_unavailable` — the
/// feature the file enables is absent, not its authority loosened.
pub fn read_private(state_dir: &Path, name: &str) -> Result<Vec<u8>> {
    let dir = dir(state_dir);
    check_dir(&dir)?;
    let path = dir.join(name);
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
        .map_err(|e| {
            if e.raw_os_error() == Some(libc::ELOOP) {
                Error::invalid(
                    "capability_unavailable",
                    format!("{} is a symlink — refusing it", path.display()),
                )
            } else {
                Error::invalid("capability_unavailable", format!("{}: {e}", path.display()))
            }
        })?;
    let md = file.metadata()?;
    if !md.is_file() {
        return Err(Error::invalid(
            "capability_unavailable",
            format!("{} is not a regular file — refusing it", path.display()),
        ));
    }
    if md.uid() != euid() {
        return Err(Error::invalid(
            "capability_unavailable",
            format!(
                "{} is owned by uid {}, not this user (uid {}) — refusing it",
                path.display(),
                md.uid(),
                euid()
            ),
        ));
    }
    if md.mode() & 0o077 != 0 {
        return Err(Error::invalid(
            "capability_unavailable",
            format!(
                "Path {} has mode {:03o}; it must be private — run {}",
                crate::printed_command::shown(&path),
                md.mode() & 0o777,
                crate::printed_command::with_path("chmod 600", &path, "")
            ),
        ));
    }
    let mut bytes = Vec::new();
    (&mut file).take(1 << 20).read_to_end(&mut bytes)?;
    Ok(bytes)
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
    /// Minted for another origin or audience (board vs the pinned entry
    /// origin, or loopback vs tailnet).
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
                "this login link was minted for another address or audience \
                 (board vs trusted credential origin, or loopback vs tailnet) — \
                 run the matching `cadence ui login` for the page you opened"
            }
        }
    }
}

/// A public session's named user (CAD-526): the identity a verified
/// AgenticOS assertion carried. `handle` is the `[A-Za-z0-9_-]`
/// attribution that fits the comment-author and monitor-ack grammars;
/// `role` is the mapped Cadence role — `operator` or `member`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardUser {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub handle: String,
}

impl BoardUser {
    /// Is this an `owner`-mapped session — allowed the operator-only
    /// write routes?
    pub fn is_operator(&self) -> bool {
        self.role == "operator"
    }

    /// The display/audit actor: `Fable Chen <fable@example.com> (board)`,
    /// printable-ASCII only — it lands in commit `Actor:` trailers — and at
    /// most [`ACTOR_MAX`] characters, so the record's `"<actor> via board"`
    /// source fits the store's 200 (CAD-1300). A name with no ASCII
    /// characters shows the handle. An email over 100 characters keeps its
    /// first 91, `~` and 8 hex digits of the sha256 of the full email (so
    /// two long emails stay distinct); the name is cut to the room left.
    pub fn actor(&self) -> String {
        let clean = |raw: &str| -> String {
            raw.chars()
                .filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '<' && *c != '>')
                .collect::<String>()
                .trim()
                .to_string()
        };
        let mut email = clean(&self.email);
        if email.len() > 100 {
            email = format!("{}~{}", &email[..91], &digest(&self.email)[..8]);
        }
        let name = match clean(&self.name) {
            n if n.is_empty() => self.handle.clone(),
            n => n,
        };
        let email_part = if email.is_empty() {
            String::new()
        } else {
            format!(" <{email}>")
        };
        let room = ACTOR_MAX - " (board)".len() - email_part.len();
        let name = name.get(..room).unwrap_or(&name).trim_end();
        format!("{name}{email_part} (board)")
    }
}

/// Longest rendered [`BoardUser::actor`]: with `" via board"` it fits the
/// store's 200-character source limit.
const ACTOR_MAX: usize = 190;

/// CAD-1374: the platform realm a public session was verified under —
/// the board's issuer, the company it serves and its public host. Saved
/// from the same config the assertion was checked against, never re-read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicRealm {
    pub issuer: String,
    pub company: String,
    pub host: String,
}

/// The verified platform tenancy a public session was opened under
/// (CAD-1412): the assertion's `iss` and `company`, which the daemon
/// checked against the configured board identity before this value
/// existed. Absent on every legacy or non-public session, so a session
/// without it can never authorize a human-threads actor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HumanScope {
    pub issuer: String,
    pub organization_id: String,
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
    /// The platform-asserted user a `public` session names (CAD-526).
    pub user: Option<BoardUser>,
    /// The verified tenancy of a scoped public session (CAD-1412).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<HumanScope>,
    /// Non-secret durable account handle; absent on legacy sessions.
    pub account_id: Option<String>,
}

/// A freshly opened session: the token goes to the board once, for its
/// `Set-Cookie`, and the key once, for the page — neither is stored or
/// returned again.
pub struct Opened {
    pub token: String,
    /// The session's second credential (review round 2, PR #249): the
    /// page keeps it in this browser's `localStorage` — scoped to its
    /// exact origin, port included — and sends it as
    /// `X-Cadence-Session` on every write. A cookie leaked to another
    /// port's listener is worthless without it, and a cross-site request
    /// cannot set the header.
    pub key: String,
    pub session: SessionView,
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

    /// These paths used to print unquoted, so a crafted name ran as a command even
    /// from the decoded message.
    #[test]
    fn a_loose_dir_prints_chmod_only_for_a_path_safe_to_paste() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("has;semi colon");
        fs::create_dir(&state).unwrap();
        ensure_secret(&state).unwrap();
        chmod(&dir(&state), 0o755);
        let shown = read_secret(&state).unwrap_err().to_string();
        let quoted = format!("`chmod 700 '{}'`", dir(&state).display());
        assert!(shown.contains(&quoted), "{shown}");

        let state = root.path().join("it's");
        fs::create_dir(&state).unwrap();
        ensure_secret(&state).unwrap();
        chmod(&dir(&state), 0o755);
        let shown = read_secret(&state).unwrap_err().to_string();
        assert!(shown.contains("`chmod 700 VALUE_1`"), "{shown}");
        assert!(shown.contains("cannot be printed"), "{shown}");

        for name in crate::printed_command::tests::CRAFTED {
            let state = root.path().join(name);
            fs::create_dir(&state).unwrap();
            ensure_secret(&state).unwrap();
            chmod(&dir(&state), 0o755);
            let shown = read_secret(&state).unwrap_err().to_string();
            crate::printed_command::tests::assert_inert_when_pasted(&shown);

            // A malformed secret, and a symlink where the secret belongs.
            chmod(&dir(&state), 0o700);
            let path = secret_path(&state);
            fs::write(&path, "not a secret").unwrap();
            let shown = read_secret(&state).unwrap_err().to_string();
            assert!(shown.contains("well-formed"), "{shown}");
            crate::printed_command::tests::assert_inert_when_pasted(&shown);

            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(root.path().join("elsewhere"), &path).unwrap();
            let shown = read_secret(&state).unwrap_err().to_string();
            assert!(shown.contains("symlink"), "{shown}");
            crate::printed_command::tests::assert_inert_when_pasted(&shown);
        }
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
}
