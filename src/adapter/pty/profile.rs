//! The per-TUI contract behind [`super::PtyAdapter`].
//!
//! A profile carries every provider-specific fact the generic owned-pane
//! mechanics need — launch argv, session-ownership proof, screen
//! analysis, message wording, and input hazards — and nothing more. The
//! bar for a method is "a second TUI would answer it differently"; a
//! behaviour with only one sensible implementation belongs in the
//! generic adapter, not here. Plain data is preferred over callbacks
//! wherever data is enough.

use std::time::Duration;

use crate::adapter::Probe;
use crate::error::{Error, Result};

/// One provider terminal UI, for a generic owned tmux pane.
pub trait TuiProfile: Send + Sync {
    /// Display name interpolated into operator-facing messages
    /// ("Devin").
    fn name(&self) -> &'static str;

    /// Mint a fresh native session id before the pane spawns, called
    /// only when the agent has no stored session. A returned id is
    /// recorded (`cadence/session_minted` → `params.session`) *before*
    /// the launch runs, so a failed or later open resumes the same id
    /// instead of abandoning a mint per respawn. Profiles whose TUI
    /// registers its own session (the pane's owned-session discovery
    /// adopts whatever it gets) return `None` — the default.
    fn prepare_session(&self) -> Result<Option<String>> {
        Ok(None)
    }

    /// Whether a stored native session id may be discarded when a
    /// resume proves it can never come back — true only for providers
    /// whose sessions are cheap mintable ids (cursor chats). Claude
    /// and Devin sessions can name a real conversation the operator
    /// chose, so a transient proof failure must never drop one. When
    /// this returns false the adapter never emits
    /// `cadence/session_resume_failed` and the daemon refuses to clear
    /// `params.session` for the endpoint either.
    fn session_is_disposable(&self) -> bool {
        false
    }

    /// Shell command the pane runs: a fresh launch, or a resume of the
    /// native `session` — the resume flag's shape is the profile's own
    /// (Devin: `-r <session>`).
    fn launch_command(&self, resume: Option<&str>) -> Result<String>;

    /// Notice the pane shell prints when the TUI exits — keeps a dead
    /// pane visible briefly instead of dropping to a bare shell that
    /// would accept input meant for the TUI.
    fn exit_banner(&self) -> &'static str;

    /// The native session a pane currently owns, discovered from the
    /// pane's process id. The mechanism is the profile's own (Devin:
    /// the session-lock flock held by a pane descendant).
    fn owned_session(&self, pane_pid: u32) -> Option<String>;

    /// Session resolution at open: given the session the agent wants
    /// (`desired`) and the one the pane actually owns (`found`), return
    /// the native session to adopt or refuse. Also validates a session
    /// acquired during a fresh launch's open wait — a mismatch there is
    /// the same "wrong session" refusal, never an adoption.
    fn resolve_session(&self, desired: Option<&str>, found: Option<String>) -> Result<String>;

    /// Pre-launch refusal: reject when `session` is already owned
    /// outside our (future) pane — Cadence never takes over a foreign
    /// terminal. Profiles that cannot prove foreign ownership return
    /// `Ok(())`.
    fn refuse_takeover(&self, session: &str) -> Result<()>;

    /// Gate check: prove the pane still owns `native`. The error text
    /// is the profile's — the proof mechanism is its own.
    fn verify_ownership(&self, native: &str, pane_pid: u32) -> Result<()>;

    /// Bound on the TUI acquiring its native session after launch.
    fn open_deadline(&self) -> Duration;

    /// Reduce a captured screen to gate facts for this TUI. `cursor` is
    /// the pane cursor cell `(x, y)` when the adapter could read it —
    /// some TUIs render dim "ghost" suggestion text in an empty input
    /// line that a plain-text capture cannot tell from a staged draft;
    /// the suggestion never moves the cursor, so a profile uses the
    /// cursor to tell them apart. `None` means unknown — treat any
    /// visible draft text as real (conservative).
    fn analyze(&self, screen: &str, cursor: Option<(u32, u32)>) -> Probe;

    /// [`Self::analyze`] over a styled capture (`capture-pane -e`: SGR
    /// attributes kept) — what the adapter actually probes with. The
    /// default strips the attributes and analyzes the plain text; a
    /// profile whose input line carries meaning in its attributes (the
    /// Claude TUI's dim prompt suggestion) overrides it.
    fn analyze_styled(&self, styled: &str, cursor: Option<(u32, u32)>) -> Probe {
        self.analyze(&super::sgr::strip(styled), cursor)
    }

    /// `agent respond` rejection text — approvals are answered in the
    /// terminal itself; the message names the provider's prompt.
    fn respond_rejection(&self) -> &'static str;

    /// `agent answer`: translate a menu `choice` (the option's printed
    /// index) into the keystrokes this TUI's open approval menu
    /// accepts, validated against the rows on `screen` — a numbered
    /// menu takes its digit key, a lettered one its hotkey, an
    /// unnumbered select arrows + Enter. The default refuses: a TUI
    /// with no menu-answer keymap is answered in the terminal itself.
    fn approval_answer(&self, _screen: &str, choice: &str) -> Result<Vec<String>> {
        let _ = choice;
        Err(Error::rejected(
            "this TUI has no approval-menu keymap — answer it in the terminal itself",
        ))
    }

    /// tmux key names that interrupt this TUI's running turn — the
    /// provider's own stop, the one its busy hint names (CAD-323). The
    /// default is `C-c`; a TUI whose `C-c` also clears or exits
    /// overrides it with its documented interrupt key.
    fn interrupt_keys(&self) -> &'static [&'static str] {
        &["C-c"]
    }

    /// `agent recover-submit` (CAD-152): the staged draft's visible
    /// rows, top to bottom, with this TUI's chrome (prompt glyph, box
    /// rules, indentation) removed — the text the generic matcher
    /// compares against the message body. Called only after the probe
    /// saw a non-empty input line. `Err` names why the draft cannot be
    /// delimited on this screen, and the recovery refuses: the default
    /// refuses for every TUI whose input area has no proven shape, so a
    /// new profile fails closed until it opts in.
    fn draft_rows(&self, _styled: &str) -> std::result::Result<Vec<String>, String> {
        Err(format!(
            "{} drafts cannot be read reliably from the screen — recover it in \
             the terminal (`cadence agent attach`)",
            self.name()
        ))
    }

    /// First non-space characters that must never be pasted verbatim.
    /// Terminal UIs commonly treat a leading `/` or `!` as a command or
    /// mode switch, so a literal paste of such a body is an injection
    /// path — the adapter rejects it `PreWrite` before any byte reaches
    /// the pane. An empty list asserts the TUI has no such hazard.
    fn forbidden_prefixes(&self) -> &'static [char];
}
