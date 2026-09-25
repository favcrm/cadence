//! CAD-482: a test-only caller-identity seam for fixture daemons and
//! boards.
//!
//! Operator-gated tests run inside agent panes, where the production
//! proof ([`crate::peer::operator_proof`]) correctly refuses the
//! runner's ancestry — and where `CADENCE_ALIAS` leaks an ambient
//! identity into every spawned process (the F14 case: a suite behaved
//! differently in a pane and in CI). The seam removes both problems by
//! making test identity **explicit**: a test says which identity a call
//! runs as — the operator, a named agent, or provably-no-one — and the
//! fixture honors exactly that, never the process's environment or
//! ancestry. Production binaries cannot contain it:
//! [`compile_error!`] below keeps the feature out of every release
//! build, and CI's release build is asserted to carry no seam symbols.
//!
//! Wire surface (all honored only while a fixture is armed):
//!
//! - `test_caller` — a frame-level field on a daemon request,
//!   `{"token": <fixture token>, "as": "operator" | "unproven" |
//!   "agent:<alias>"}`. It sits beside `method`/`params`, never inside
//!   `params`, so no handler or identity-field check can be tricked
//!   into reading it as a request argument.
//! - `X-Cadence-Test-As` + `X-Cadence-Test-Token` — the same assertion
//!   on a board's HTTP request.
//! - `CADENCE_TEST_SEAM` — arms a spawned `daemon run` / `ui run`
//!   fixture process.
//! - `CADENCE_TEST_AS` — the asserted identity of a *spawned* binary's
//!   outgoing daemon calls (a test's `cadence` CLI, a fixture board
//!   process). It is read by [`imp::caller_frame`]; an in-process
//!   caller uses [`scoped`] instead, which cannot leak across tests.
//!
//! Confinement (acceptance: "refuses the production state dir or any
//! socket outside a temp root, and says why"): [`imp::arm`] refuses a
//! state dir that is the resolved production default
//! ([`crate::client::default_state_dir`]) or that does not sit under
//! the process's temp root — a seam daemon can never be pointed at
//! `~/.local/state/cadence`, and its socket lives under the same
//! checked dir. The per-instance token (`<state>/seam/token`, mode
//! 0600) binds assertions to that fixture: a caller must read the
//! fixture's own credential to assert, so assertions can never
//! cross-attest onto a daemon that did not opt in.
//!
//! Identity scope: the assertion rides the request itself
//! (`handle_conn` enters a thread-local scope for the dispatch), so
//! two threads of one test process may assert different identities on
//! one connection without interference, and a caller that asserts
//! nothing keeps the unmodified production derivation — unproven in a
//! pane, operator in CI, exactly as before.

#[cfg(all(feature = "test-seam", not(debug_assertions)))]
compile_error!(
    "feature `test-seam` arms a caller-identity override that bypasses \
     /proc ancestry proof; it is for test binaries only and must never \
     be compiled into a release build (CAD-482)"
);

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::error::{Error, Result};

/// Environment variable that arms a spawned fixture process
/// (`daemon run`, `ui run`). Read only in test-seam builds.
pub const ARM_ENV: &str = "CADENCE_TEST_SEAM";
/// Environment variable carrying a spawned binary's asserted identity
/// for its outgoing daemon calls — the value is [`Asserted`]'s wire
/// form (`operator`, `unproven`, `agent:<alias>`).
pub const AS_ENV: &str = "CADENCE_TEST_AS";
/// The daemon request frame's assertion field.
pub const FRAME_FIELD: &str = "test_caller";
/// Board headers carrying the assertion and the fixture credential.
pub const AS_HEADER: &str = "X-Cadence-Test-As";
pub const TOKEN_HEADER: &str = "X-Cadence-Test-Token";

/// Who a test says a caller runs as. The wire form is a single string:
/// `operator`, `unproven`, or `agent:<alias>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asserted {
    /// The operator — as if `peer::operator_proof` passed on the
    /// caller's real ancestry.
    Operator,
    /// The registered agent `alias` — as if that pane or managed
    /// endpoint sat on the caller's ancestry.
    Agent(String),
    /// Provably no-one: the refusal `Who::Unproven` produces, asserted
    /// directly so it is the same in a pane and in CI.
    Unproven,
}

impl Asserted {
    /// The wire form.
    pub fn as_str(&self) -> String {
        match self {
            Asserted::Operator => "operator".to_string(),
            Asserted::Agent(alias) => format!("agent:{alias}"),
            Asserted::Unproven => "unproven".to_string(),
        }
    }
}

/// [`Asserted`]'s wire form: `operator`, `unproven`, `agent:<alias>`.
/// `Err` names the accepted spellings.
pub fn parse_as(value: &str) -> Result<Asserted> {
    match value {
        "operator" => Ok(Asserted::Operator),
        "unproven" => Ok(Asserted::Unproven),
        _ => match value.strip_prefix("agent:") {
            Some(alias) if !alias.is_empty() => Ok(Asserted::Agent(alias.to_string())),
            _ => Err(Error::rejected(format!(
                "test seam: '{value}' is not an identity — assert 'operator', \
                 'unproven', or 'agent:<alias>'"
            ))),
        },
    }
}

/// An armed fixture: its state dir (canonicalized at arm time) and the
/// credential it serves. The token is read from `<state>/seam/token`
/// per check, so a board attached before its daemon minted, or a
/// daemon restarted on the same dir, always sees the live credential.
#[derive(Clone)]
pub struct Seam {
    /// The armed fixture's canonical state dir. Read by [`admits`],
    /// which only exists on the feature — unused without it.
    #[cfg_attr(not(feature = "test-seam"), allow(dead_code))]
    state_dir: PathBuf,
}

impl Seam {
    /// Where asserting callers read the credential.
    pub fn token_path(state_dir: &Path) -> PathBuf {
        state_dir.join("seam").join("token")
    }

    /// The fixture's minted token, if a daemon armed this state dir —
    /// `None` when absent or unreadable. Callers prove fixture access
    /// by presenting it.
    pub fn token_at(state_dir: &Path) -> Option<String> {
        let token = std::fs::read_to_string(Self::token_path(state_dir)).ok()?;
        let token = token.trim().to_string();
        (!token.is_empty()).then_some(token)
    }

    /// The armed side's check: the presented `test_caller.token` must
    /// be the token minted in this state dir right now.
    #[cfg_attr(not(feature = "test-seam"), allow(dead_code))]
    fn admits(&self, token: Option<&str>) -> Result<()> {
        match (Self::token_at(&self.state_dir), token) {
            (Some(minted), Some(presented)) if minted == presented => Ok(()),
            (None, _) => Err(Error::rejected(format!(
                "test seam: {} carries no minted credential — a seam-armed \
                 daemon on this state dir writes it",
                self.state_dir.display()
            ))),
            _ => Err(Error::rejected(
                "test seam: 'test_caller' needs this fixture's seam token \
                 (read <state>/seam/token — it is minted per fixture run)",
            )),
        }
    }
}

/// Is `state_dir` a seam-armed fixture right now? Test helpers pick the
/// asserted path over the detached-process one on this answer.
pub fn armed(state_dir: &Path) -> bool {
    cfg!(feature = "test-seam") && Seam::token_at(state_dir).is_some()
}

/// `Err` when `requested` but the seam cannot arm — loudly, so a
/// fixture never silently falls back to ambient identity.
pub fn arm_if_requested(state_dir: &Path, requested: bool) -> Result<Option<Seam>> {
    if !requested {
        return Ok(None);
    }
    imp::arm(state_dir).map(Some)
}

/// The board's variant: the daemon mints the credential, an armed
/// board *attaches* to it — with no minted token there is nothing to
/// validate assertions against, so starting one refuses.
pub fn attach_if_requested(state_dir: &Path, requested: bool) -> Result<Option<Seam>> {
    if !requested {
        return Ok(None);
    }
    imp::attach(state_dir).map(Some)
}

/// `run` a closure with `who` as this thread's asserted caller — the
/// in-process assertion channel. Outgoing `client::rpc` calls inside
/// `f` carry `test_caller`; a daemon dispatch under it resolves `who`.
#[cfg(feature = "test-seam")]
pub fn scoped<T>(who: Asserted, f: impl FnOnce() -> T) -> T {
    let _scope = imp::Scope::set(Some(who));
    f()
}

/// Without the feature there is no assertion to make.
#[cfg(not(feature = "test-seam"))]
pub fn scoped<T>(_who: Asserted, f: impl FnOnce() -> T) -> T {
    f()
}

/// The assertion in effect on this thread (`None` outside a scope).
/// Daemon-side derivation and `client::rpc` consult this; without the
/// feature it is always `None` and every consult compiles out.
pub fn asserted() -> Option<Asserted> {
    imp::asserted()
}

/// The `test_caller` field this thread's outgoing `client::rpc` calls
/// should carry — the scoped assertion (precise, must match the
/// target's credential) or the process's [`AS_ENV`] (blunt: only
/// attached when the target actually arms, so one env cannot make an
/// unarmed daemon refuse the call).
pub fn caller_frame(state_dir: &Path) -> Result<Option<Value>> {
    imp::caller_frame(state_dir)
}

/// Daemon-side: resolve a request frame's `test_caller` against the
/// armed `seam`. The returned [`imp::Scope`] makes the asserted
/// identity visible for the duration of one dispatch.
pub fn scope_frame(seam: Option<&Seam>, frame: &Value) -> Result<imp::Scope> {
    imp::scope_frame(seam, frame)
}

/// Board-side: the request's `X-Cadence-Test-As`/`X-Cadence-Test-Token`
/// headers as a scope — `Err` refuses the request (a half-present or
/// forged header fails loudly, never falls back to ambient).
pub fn scope_headers(
    seam: Option<&Seam>,
    as_value: Option<&str>,
    token: Option<&str>,
) -> std::result::Result<imp::Scope, String> {
    imp::scope_headers(seam, as_value, token)
}

/// `daemon run`'s arming env, feature-gated at the call site.
pub fn env_armed() -> bool {
    cfg!(feature = "test-seam") && std::env::var_os(ARM_ENV).is_some()
}

// ---------------- implementation ----------------

/// The dispatch/request scope a seam assertion runs under —
/// [`scope_frame`] and [`scope_headers`] return it; dropping it
/// restores the thread's previous assertion.
pub use imp::Scope;

#[cfg(feature = "test-seam")]
mod imp {
    use super::*;
    use serde_json::json;

    /// Release-canary (CAD-482): this byte string exists in the binary
    /// only when the seam is compiled in. `compile_error!` above blocks
    /// `test-seam` on a release profile; the CI release job greps the
    /// built binary for `cadence-test-seam-v1` to prove neither the
    /// flag nor this code ever shipped — removing the compile_error
    /// and building `--release --features test-seam` is the mutation
    /// this catches.
    #[used]
    static RELEASE_CANARY: &[u8] = b"cadence-test-seam-v1\n";

    std::thread_local! {
        /// The asserting scope the current dispatch/request runs under.
        static ASSERTED: std::cell::RefCell<Option<Asserted>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Restores the previous assertion on drop — scopes nest, so a
    /// relay under an asserted request re-asserts rather than leaks.
    pub struct Scope(Option<Asserted>);

    impl Scope {
        pub fn set(who: Option<Asserted>) -> Scope {
            Scope(ASSERTED.with(|c| c.replace(who)))
        }
    }

    impl Drop for Scope {
        fn drop(&mut self) {
            ASSERTED.with(|c| c.replace(self.0.take()));
        }
    }

    pub fn asserted() -> Option<Asserted> {
        ASSERTED.with(|c| c.borrow().clone())
    }

    /// The confinement check both arm paths share: canonicalize
    /// `state_dir`, refuse the production default and anything outside
    /// the temp root, return the canonical dir.
    fn confine(state_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(state_dir)?;
        let canonical = state_dir.canonicalize().map_err(|e| {
            Error::internal(format!("test seam: cannot canonicalize state dir: {e}"))
        })?;
        if let Ok(default) = crate::client::default_state_dir().and_then(|d| {
            d.canonicalize()
                .map_err(|e| Error::internal(format!("default state dir: {e}")))
        }) {
            if canonical == default {
                return Err(Error::rejected(format!(
                    "test seam refused: '{}' is the default state dir — the seam \
                     never serves the production state dir, even under a test \
                     HOME. Point the fixture at its own temp dir",
                    canonical.display()
                )));
            }
        }
        let temp_root = std::env::temp_dir().canonicalize().map_err(|e| {
            Error::internal(format!("test seam: cannot canonicalize temp root: {e}"))
        })?;
        if !canonical.starts_with(&temp_root) {
            return Err(Error::rejected(format!(
                "test seam refused: state dir '{}' is not under the test temp \
                 root '{}' — the seam confines fixtures to temp state",
                canonical.display(),
                temp_root.display()
            )));
        }
        // The daemon socket lives inside the checked state dir; name it
        // in the check so the refusal reads the whole confinement.
        let socket = crate::client::socket_path(&canonical);
        if !socket.starts_with(&temp_root) {
            return Err(Error::rejected(format!(
                "test seam refused: socket '{}' would sit outside the test temp \
                 root '{}'",
                socket.display(),
                temp_root.display()
            )));
        }
        Ok(canonical)
    }

    /// Arm a fixture on `state_dir`: refuse the production state dir
    /// and anything outside the temp root, then mint the token callers
    /// present. Runs before the store opens, so a refused start leaves
    /// nothing behind.
    pub fn arm(state_dir: &Path) -> Result<Seam> {
        let canonical = confine(state_dir)?;
        let dir = canonical.join("seam");
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes)
            .map_err(|e| Error::internal(format!("test seam: token mint: {e}")))?;
        let token = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let path = Seam::token_path(&canonical);
        std::fs::write(&path, format!("{token}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Seam {
            state_dir: canonical,
        })
    }

    /// An armed board attaches to the token its daemon mints — same
    /// confinement, no mint of its own. The token resolves per request,
    /// so a board may start before its daemon.
    pub fn attach(state_dir: &Path) -> Result<Seam> {
        Ok(Seam {
            state_dir: confine(state_dir)?,
        })
    }

    /// The `test_caller` field for an outgoing call — see the outer
    /// doc. A scoped assertion against an unarmed target errs (the test
    /// meant it); an [`AS_ENV`] assertion on an unarmed target attaches
    /// nothing (the process-wide env cannot name one state dir).
    pub fn caller_frame(state_dir: &Path) -> Result<Option<Value>> {
        let scoped = asserted();
        let env = || std::env::var(AS_ENV).ok().and_then(|v| parse_as(&v).ok());
        match (scoped, env()) {
            (None, None) => Ok(None),
            (Some(who), _) => {
                let token = Seam::token_at(state_dir).ok_or_else(|| {
                    Error::rejected(format!(
                        "test seam: this call asserts '{}' but {} is not a \
                         seam-armed fixture (no {})",
                        who.as_str(),
                        state_dir.display(),
                        Seam::token_path(state_dir).display()
                    ))
                })?;
                Ok(Some(json!({"token": token, "as": who.as_str()})))
            }
            (None, Some(who)) => match Seam::token_at(state_dir) {
                Some(token) => Ok(Some(json!({"token": token, "as": who.as_str()}))),
                None => Ok(None),
            },
        }
    }

    /// Resolve `test_caller` on a request frame into the dispatch
    /// scope. An unarmed daemon refuses the field; a wrong token
    /// refuses; anything else the frame asserts becomes this
    /// dispatch's caller.
    pub fn scope_frame(seam: Option<&Seam>, frame: &Value) -> Result<Scope> {
        let Some(field) = frame.get(FRAME_FIELD) else {
            return Ok(Scope::set(None));
        };
        let Some(seam) = seam else {
            return Err(Error::rejected(
                "request field 'test_caller' is honored only on a daemon started \
                 with the test seam armed (CAD-482)",
            ));
        };
        seam.admits(field.get("token").and_then(Value::as_str))?;
        let who = parse_as(
            field
                .get("as")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::rejected("test seam: 'test_caller.as' is required"))?,
        )?;
        Ok(Scope::set(Some(who)))
    }

    /// The board's request headers → a request scope (see
    /// [`scope_headers`]). A board with no seam armed refuses the
    /// headers outright, matching the daemon's rule.
    pub fn scope_headers(
        seam: Option<&Seam>,
        as_value: Option<&str>,
        token: Option<&str>,
    ) -> std::result::Result<Scope, String> {
        if as_value.is_none() && token.is_none() {
            return Ok(Scope::set(None));
        }
        let (Some(as_value), Some(token)) = (as_value, token) else {
            return Err(format!(
                "test seam: {AS_HEADER} and {TOKEN_HEADER} travel together — \
                 one arrived without the other"
            ));
        };
        let Some(seam) = seam else {
            return Err(
                "test seam: assertion headers are honored only on a board started \
                 with the test seam armed (CAD-482)"
                    .to_string(),
            );
        };
        seam.admits(Some(token)).map_err(|e| e.to_string())?;
        let who = parse_as(as_value).map_err(|e| e.to_string())?;
        Ok(Scope::set(Some(who)))
    }
}

#[cfg(not(feature = "test-seam"))]
mod imp {
    use super::*;

    /// The tokenless no-op: [`asserted`] is always `None`, so every
    /// derivation consult compiles out.
    pub struct Scope;

    impl Scope {
        pub fn set(_: Option<Asserted>) -> Scope {
            Scope
        }
    }

    pub fn asserted() -> Option<Asserted> {
        None
    }

    /// The seam is not in this build: an assertion field is refused
    /// loudly rather than silently ignored.
    pub fn caller_frame(_state_dir: &Path) -> Result<Option<Value>> {
        if std::env::var_os(AS_ENV).is_some() {
            return Err(Error::rejected(format!(
                "{AS_ENV} is set but this build has no test seam — build with \
                 `--features test-seam` (test binaries only)"
            )));
        }
        Ok(None)
    }

    pub fn scope_frame(_seam: Option<&Seam>, frame: &Value) -> Result<Scope> {
        if frame.get(FRAME_FIELD).is_some() {
            return Err(Error::rejected(
                "request field 'test_caller' exists only in a `test-seam` build \
                 (CAD-482) — production binaries never carry it",
            ));
        }
        Ok(Scope::set(None))
    }

    pub fn scope_headers(
        _seam: Option<&Seam>,
        as_value: Option<&str>,
        token: Option<&str>,
    ) -> std::result::Result<Scope, String> {
        if as_value.is_some() || token.is_some() {
            return Err(format!(
                "test seam headers are honored only in a `test-seam` build \
                 (CAD-482)"
            ));
        }
        Ok(Scope::set(None))
    }

    /// Arming without the feature is a hard error — a fixture asking
    /// for the seam must never degrade to ambient identity silently.
    pub fn arm(_state_dir: &Path) -> Result<Seam> {
        Err(Error::rejected(
            "the test seam was requested but this binary was built without \
             feature `test-seam` — test binaries only (CAD-482)",
        ))
    }

    /// Same refusal for the board attach path.
    pub fn attach(_state_dir: &Path) -> Result<Seam> {
        Err(Error::rejected(
            "the test seam was requested but this binary was built without \
             feature `test-seam` — test binaries only (CAD-482)",
        ))
    }
}
