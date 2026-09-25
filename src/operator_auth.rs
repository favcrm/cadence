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
                "{} has mode {:03o}; it must be private — run `chmod 600 {}`",
                path.display(),
                md.mode() & 0o777,
                path.display()
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
    /// printable-ASCII only — it lands in commit `Actor:` trailers.
    pub fn actor(&self) -> String {
        let clean = |raw: &str| -> String {
            raw.chars()
                .filter(|c| c.is_ascii() && !c.is_ascii_control() && *c != '<' && *c != '>')
                .take(120)
                .collect::<String>()
                .trim()
                .to_string()
        };
        let (name, email) = (clean(&self.name), clean(&self.email));
        match (name.is_empty(), email.is_empty()) {
            (false, false) => format!("{name} <{email}> (board)"),
            (false, true) => format!("{name} (board)"),
            _ => format!("{} (board)", self.handle),
        }
    }
}

/// One stored session: the token's hash and what `ui sessions` shows.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Row {
    hash: String,
    /// `sha256(key)` of the session's second credential — the value the
    /// page sends as `X-Cadence-Session`. A row without one (written
    /// before it existed, or a public session — the contract's cookie
    /// is the whole credential, CAD-526) never matches the keyed check.
    #[serde(default)]
    key_hash: String,
    origin: Origin,
    created: i64,
    last_used: i64,
    expires_at: i64,
    #[serde(default)]
    user_agent: String,
    /// The named user a public session belongs to — `None` for the
    /// operator's loopback/tailnet sessions.
    #[serde(default)]
    user: Option<BoardUser>,
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
            user: self.user.clone(),
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
    /// The platform-asserted user a `public` session names (CAD-526).
    pub user: Option<BoardUser>,
}

/// A freshly opened session: the token goes to the board once, for its
/// `Set-Cookie`, and the key once, for the page — neither is stored or
/// returned again.
pub struct Opened {
    pub token: String,
    /// The session's second credential (review round 2, PR #249): the
    /// page keeps it in `sessionStorage` — scoped to its exact origin,
    /// port included — and sends it as `X-Cadence-Session` on every
    /// write. A cookie leaked to another port's listener is worthless
    /// without it, and a cross-site request cannot set the header.
    pub key: String,
    pub session: SessionView,
}

#[derive(Serialize, Deserialize, Default)]
struct SessionsFile {
    sessions: Vec<Row>,
    /// `sha256(jti)` → the assertion's `exp`: platform assertions are
    /// single-use (CAD-526); persisted so a restart inside the 60 s
    /// window cannot reopen a replay. Pruned once `exp` passes.
    #[serde(default)]
    jtis: HashMap<String, i64>,
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
    /// seen jti hash → its assertion's `exp` — the authoritative
    /// single-use memory for platform sign-ins (CAD-526).
    jtis: HashMap<String, i64>,
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
        let file = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<SessionsFile>(&b).ok())
            .unwrap_or_default();
        Self {
            path,
            sessions: file
                .sessions
                .into_iter()
                .filter(|r| well_formed(&r.hash))
                .collect(),
            links: HashMap::new(),
            spent: HashMap::new(),
            jtis: file.jtis,
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
            jtis: self.jtis.clone(),
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
        // A jti stays until its assertion's own expiry — then a replay
        // fails "expired" before the single-use check anyway.
        self.jtis.retain(|_, exp| now <= *exp);
        let before = self.sessions.len();
        self.sessions.retain(|r| r.live(now));
        before != self.sessions.len()
    }

    /// Mint a single-use link nonce for `origin`. The caller has already
    /// proved the operator; this only records `sha256(nonce)`. `public`
    /// is refused outright — those sessions are born only of a verified
    /// platform assertion, never a login link (CAD-526).
    pub fn mint(&mut self, origin: Origin, now: i64) -> Result<String> {
        if origin == Origin::Public {
            return Err(Error::invalid(
                "invalid_request",
                "public sessions open through a verified assertion, not a login link",
            ));
        }
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
        let key = random_credential()?;
        let row = Row {
            hash: digest(&token),
            key_hash: digest(&key),
            origin,
            created: now,
            last_used: now,
            expires_at: now + ABSOLUTE_SECS,
            user_agent: clean_user_agent(user_agent),
            user: None,
        };
        let session = row.view();
        self.sessions.push(row);
        self.persist()?;
        Ok(Opened {
            token,
            key,
            session,
        })
    }

    /// Open a `public` session for a verified platform user (CAD-526).
    /// `None` is the refusal — the `jti` was seen before (the daemon
    /// maps it to the contract's `assertion_replayed`).
    pub fn open_public(
        &mut self,
        user: BoardUser,
        jti: &str,
        jti_exp: i64,
        user_agent: &str,
        now: i64,
    ) -> Result<Option<Opened>> {
        self.prune(now);
        // The authoritative single-use check (contract §4/§9): verified
        // jtis persist until `exp` — a restart inside the 60 s window
        // still knows the assertion is spent.
        if self.jtis.contains_key(&digest(jti)) {
            return Ok(None);
        }
        let token = random_credential()?;
        // One session per `sub` (contract §9): a fresh sign-in ends the
        // user's earlier one — named users never stack sessions.
        self.sessions.retain(|r| {
            !(r.origin == Origin::Public && r.user.as_ref().is_some_and(|u| u.sub == user.sub))
        });
        let row = Row {
            hash: digest(&token),
            // The cookie alone is the credential on the public surface —
            // there is no page key. An empty hash can never satisfy the
            // keyed `check`.
            key_hash: String::new(),
            origin: Origin::Public,
            created: now,
            last_used: now,
            expires_at: now + PUBLIC_SESSION_SECS,
            user_agent: clean_user_agent(user_agent),
            user: Some(user),
        };
        let session = row.view();
        self.sessions.push(row);
        self.jtis.insert(digest(jti), jti_exp);
        self.persist()?;
        Ok(Some(Opened {
            token,
            key: String::new(),
            session,
        }))
    }

    /// The live public session `token` names, touching its idle clock.
    /// Cookie-only — the contract's `__Host-` session has no page key,
    /// so this deliberately ignores `key`/`origin`: a public row is
    /// bound to `Origin::Public` by construction.
    pub fn check_public(&mut self, token: &str, now: i64) -> Result<Option<SessionView>> {
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
            .find(|r| r.origin == Origin::Public && same_credential(&r.hash, &hash))
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

    /// The live session `token` AND `key` name together on `origin`,
    /// touching its idle clock. Either credential alone — or a pair
    /// presented on the other origin — is no session.
    pub fn check(
        &mut self,
        token: &str,
        key: &str,
        origin: Origin,
        now: i64,
    ) -> Result<Option<SessionView>> {
        let mut dirty = self.prune(now);
        if !well_formed(token) || !well_formed(key) {
            if dirty {
                self.persist()?;
            }
            return Ok(None);
        }
        let hash = digest(token);
        let key_hash = digest(key);
        let found = self
            .sessions
            .iter_mut()
            .find(|r| {
                // Both compared in full, whatever the first says.
                let token_ok = same_credential(&r.hash, &hash);
                let key_ok = same_credential(&r.key_hash, &key_hash);
                token_ok & key_ok && r.origin == origin
            })
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

    /// Both credentials, together: the cookie's token alone, the page's
    /// key alone, or a key from another session are no session.
    #[test]
    fn a_session_needs_its_token_and_its_key() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let a = opened(&mut auth, Origin::Loopback, T0);
        let b = opened(&mut auth, Origin::Loopback, T0);
        let none = "0".repeat(64);
        assert!(auth
            .check(&a.token, &a.key, Origin::Loopback, T0)
            .unwrap()
            .is_some());
        assert!(auth
            .check(&a.token, "", Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&a.token, &none, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&a.token, &b.key, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&a.key, &a.key, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check("", &a.key, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        // Only hashes are kept, of both.
        let text = fs::read_to_string(dir(s.path()).join(SESSIONS)).unwrap();
        assert!(!text.contains(&a.key) && text.contains(&digest(&a.key)));
    }

    /// A session is bound to its origin, idles out and ends absolutely.
    #[test]
    fn sessions_check_origin_idle_and_absolute_expiry() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let o = opened(&mut auth, Origin::Loopback, T0);
        assert!(auth
            .check(&o.token, &o.key, Origin::Loopback, T0)
            .unwrap()
            .is_some());
        assert!(auth
            .check(&o.token, &o.key, Origin::Tailnet, T0)
            .unwrap()
            .is_none());
        // Idle: untouched for 24 h is gone.
        assert!(auth
            .check(&o.token, &o.key, Origin::Loopback, T0 + IDLE_SECS)
            .unwrap()
            .is_none());
        // Absolute: used every hour, still gone after 7 days.
        let o = opened(&mut auth, Origin::Loopback, T0);
        let mut now = T0;
        while now + 3600 < T0 + ABSOLUTE_SECS {
            now += 3600;
            assert!(auth
                .check(&o.token, &o.key, Origin::Loopback, now)
                .unwrap()
                .is_some());
        }
        assert!(auth
            .check(&o.token, &o.key, Origin::Loopback, T0 + ABSOLUTE_SECS)
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
                .check(&o.token, &o.key, Origin::Loopback, T0 + 1)
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
            .check(&a.token, &a.key, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&b.token, &b.key, Origin::Loopback, T0)
            .unwrap()
            .is_some());
        assert_eq!(auth.revoke_id(&b.session.id).unwrap(), 1);
        assert!(auth
            .check(&b.token, &b.key, Origin::Loopback, T0)
            .unwrap()
            .is_none());
        assert!(auth
            .check(&c.token, &c.key, Origin::Tailnet, T0)
            .unwrap()
            .is_some());
        let pending = auth.mint(Origin::Loopback, T0).unwrap();
        assert_eq!(auth.revoke_all().unwrap(), 1);
        assert!(auth
            .check(&c.token, &c.key, Origin::Tailnet, T0)
            .unwrap()
            .is_none());
        let err = auth.open(&pending, Origin::Loopback, "ua", T0);
        assert_eq!(err_kind(&err), Some(LinkRefusal::AlreadyUsed));
    }

    fn user(sub: &str, role: &str) -> BoardUser {
        BoardUser {
            sub: sub.to_string(),
            email: format!("{sub}@example.com"),
            name: format!("{sub} display"),
            role: role.to_string(),
            handle: sub.to_string(),
        }
    }

    /// CAD-526: a public session is the cookie alone — no page key — and
    /// carries its named user. The keyed `check` never admits it, on any
    /// origin, and its token is no link nonce.
    #[test]
    fn public_sessions_are_cookie_only_and_named() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let o = auth
            .open_public(user("u_1", "operator"), "jti-1", T0 + 60, "ua", T0)
            .unwrap()
            .unwrap();
        assert_eq!(o.session.origin, Origin::Public);
        assert_eq!(o.key, "");
        assert_eq!(o.session.expires_at, T0 + PUBLIC_SESSION_SECS);
        let u = o.session.user.unwrap();
        assert_eq!(u.sub, "u_1");
        assert!(u.is_operator());
        // The cookie alone checks.
        let live = auth.check_public(&o.token, T0).unwrap().unwrap();
        assert_eq!(live.id, o.session.id);
        assert_eq!(live.user.unwrap().email, "u_1@example.com");
        assert!(auth.check_public(&"0".repeat(64), T0).unwrap().is_none());
        // The keyed check never sees it — whatever key is presented,
        // whatever origin is named.
        let key = "f".repeat(64);
        for origin in [Origin::Loopback, Origin::Tailnet, Origin::Public] {
            assert!(auth.check(&o.token, &key, origin, T0).unwrap().is_none());
            assert!(auth.check(&o.token, "", origin, T0).unwrap().is_none());
        }
        // It is not a link nonce and a link can never mint `public`.
        assert_eq!(
            err_kind(&auth.open(&o.token, Origin::Loopback, "ua", T0)),
            Some(LinkRefusal::Unknown)
        );
        assert!(auth.mint(Origin::Public, T0).is_err());
    }

    /// CAD-526 §4: the `jti` is single-use, and the memory is persisted —
    /// a daemon restart inside the assertion's window still refuses the
    /// replay. Only its sha256 is stored; once `exp` passes it is
    /// forgotten (the verifier's own expiry bound is the wall).
    #[test]
    fn a_spent_jti_stays_spent_across_a_reload() {
        let s = state();
        ensure_secret(s.path()).unwrap();
        let mut auth = Auth::load(s.path());
        assert!(auth
            .open_public(user("u_1", "member"), "jti-9", T0 + 60, "ua", T0)
            .unwrap()
            .is_some());
        // Same `jti`, same instant — no second session.
        assert!(auth
            .open_public(user("u_1", "member"), "jti-9", T0 + 60, "ua", T0)
            .unwrap()
            .is_none());
        let text = fs::read_to_string(dir(s.path()).join(SESSIONS)).unwrap();
        assert!(!text.contains("jti-9"), "raw jti persisted");
        assert!(text.contains(&digest("jti-9")));
        let mut reloaded = Auth::load(s.path());
        assert!(reloaded
            .open_public(user("u_1", "member"), "jti-9", T0 + 60, "ua", T0 + 1)
            .unwrap()
            .is_none());
        // Past `exp` the entry is gone — the assertion would fail
        // verification before this check anyway.
        assert!(reloaded
            .open_public(user("u_1", "member"), "jti-9", T0 + 60, "ua", T0 + 61)
            .unwrap()
            .is_some());
    }

    /// CAD-526 §9: one session per `sub` — a fresh sign-in ends the same
    /// user's earlier one and leaves every other user's alone; the
    /// 60-minute bound is absolute, not idle-reset.
    #[test]
    fn public_sessions_replace_per_sub_and_die_at_sixty_minutes() {
        let s = state();
        let mut auth = Auth::load(s.path());
        let a = auth
            .open_public(user("u_1", "member"), "jti-a", T0 + 60, "ua", T0)
            .unwrap()
            .unwrap();
        let b = auth
            .open_public(user("u_2", "operator"), "jti-b", T0 + 60, "ua", T0)
            .unwrap()
            .unwrap();
        let a2 = auth
            .open_public(user("u_1", "member"), "jti-a2", T0 + 60, "ua", T0 + 5)
            .unwrap()
            .unwrap();
        assert!(auth.check_public(&a.token, T0 + 5).unwrap().is_none());
        assert!(auth.check_public(&a2.token, T0 + 5).unwrap().is_some());
        assert!(auth.check_public(&b.token, T0 + 5).unwrap().is_some());
        // An operator session for the same sub is never replaced — a
        // public sign-in ends only public sessions.
        let op = opened(&mut auth, Origin::Loopback, T0 + 6);
        let a3 = auth
            .open_public(user("u_1", "member"), "jti-a3", T0 + 60, "ua", T0 + 7)
            .unwrap()
            .unwrap();
        assert!(auth.check_public(&a2.token, T0 + 7).unwrap().is_none());
        assert!(auth.check_public(&a3.token, T0 + 7).unwrap().is_some());
        assert!(auth
            .check(&op.token, &op.key, Origin::Loopback, T0 + 7)
            .unwrap()
            .is_some());
        // Absolute at 60 minutes even in constant use.
        let edge = T0 + PUBLIC_SESSION_SECS;
        assert!(auth.check_public(&b.token, edge - 1).unwrap().is_some());
        assert!(auth.check_public(&b.token, edge).unwrap().is_none());
    }

    /// `BoardUser::actor` renders printable-ASCII `git`-trailer-safe
    /// attribution; empty display data falls back to the handle.
    #[test]
    fn a_board_user_actor_is_trailer_safe() {
        let mut u = user("u_1", "member");
        assert_eq!(u.actor(), "u_1 display <u_1@example.com> (board)");
        u.name = "F<b>le\nChen".into();
        u.email = "f\t@x.co>".into();
        assert_eq!(u.actor(), "FbleChen <f@x.co> (board)");
        u.name = "  ".into();
        u.email = "".into();
        assert_eq!(u.actor(), "u_1 (board)");
    }
}
