//! The stub TUI profile — a test double for the fake-TUI harness.
//!
//! It exists to prove the adapter is really profile-driven: a
//! different prompt glyph, different busy/approval markers, a
//! different launch command and a different forbidden-prefix list,
//! driven by `CADENCE_STUB_COMMAND`/`CADENCE_STUB_LOCKS` exactly the
//! way the Devin profile reads its overrides. It is not a real
//! provider; nothing outside tests should register it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::adapter::Probe;
use crate::error::{Error, Result};

use super::profile::TuiProfile;
use super::{descends_from, lock_holders, shlex_quote};

/// Bound on the stub TUI acquiring its session after launch.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// Status-region height for the stub analyzer — same anchoring rule as
/// any other profile: markers only count in the screen's bottom rows.
const STATUS_LINES: usize = 14;

/// Stub TUI screen signatures — deliberately different strings from
/// any real provider so the tests prove the adapter reads them from
/// the profile, not from a shared table.
mod stub_screen {
    /// Glyph leading the stub input line.
    pub const PROMPT: &str = "»";
    /// Empty-input placeholder.
    pub const PLACEHOLDER: &str = "stub ready";
    /// Busy watermark in the input line while a turn runs.
    pub const BUSY_PLACEHOLDER: &str = "stub busy";
    /// On-screen busy markers.
    pub const BUSY: &[&str] = &["stub working", "stub working hard"];
    /// Open approval/menu markers.
    pub const APPROVAL: &[&str] = &["stub approval", "stub menu open"];
}

/// Leading characters the stub treats as commands — deliberately
/// disjoint from the Devin list so the prefix guard proves it reads
/// the list from the profile.
pub const FORBIDDEN_PREFIXES: &[char] = &['~', ';'];

/// Reduce a stub screen to gate facts — the same algorithm shape as
/// the Devin analyzer (prompt line, staged draft, status-region
/// markers) over the stub's own signatures.
fn analyze_stub(screen: &str) -> Probe {
    let content = screen.trim_end();
    let tail: String = content
        .lines()
        .rev()
        .take(STATUS_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let approval_menu = stub_screen::APPROVAL.iter().any(|m| tail.contains(m));
    let region_busy = stub_screen::BUSY.iter().any(|m| tail.contains(m));
    let prompt_line = screen
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with(stub_screen::PROMPT));
    let prompt_visible = prompt_line.is_some();
    let draft = prompt_line
        .map(|l| {
            l.trim_start()
                .trim_start_matches(stub_screen::PROMPT)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let input_nonempty = !draft.is_empty()
        && !draft.starts_with(stub_screen::PLACEHOLDER)
        && !draft.starts_with(stub_screen::BUSY_PLACEHOLDER);
    let watermark_busy = draft.starts_with(stub_screen::BUSY_PLACEHOLDER);
    let busy_marker = region_busy || watermark_busy;
    let (idle, reason) = if approval_menu {
        (false, "approval menu is open")
    } else if busy_marker {
        (false, "tui is busy")
    } else if !prompt_visible {
        (false, "no prompt line visible")
    } else if input_nonempty {
        (false, "unsubmitted text in the input line")
    } else {
        (true, "idle")
    };
    Probe {
        idle,
        reason: reason.to_string(),
        input_nonempty,
        prompt_visible,
        busy_marker,
        approval_menu,
    }
}

/// The stub profile: env-provided argv, a `<locks>/<session>.lock`
/// session file (same ownership family as Devin's — the harness
/// exercises the real /proc scan), and the stub screen analyzer.
pub struct StubProfile {
    locks_dir: PathBuf,
    command: String,
    /// Count of `owned_session` calls still to answer with `None` —
    /// the transient "proof not yet visible" window, injected via
    /// `CADENCE_STUB_OWNED_MISS` so tests can replay it exactly.
    owned_misses: AtomicUsize,
}

impl StubProfile {
    /// Resolve from the environment: `CADENCE_STUB_LOCKS` for the
    /// session locks and `CADENCE_STUB_COMMAND` used verbatim as the
    /// launch argv — both required; there is no real `stub` binary to
    /// fall back to. `CADENCE_STUB_OWNED_MISS=N` (optional) makes the
    /// first N `owned_session` calls on this instance report nothing.
    pub fn new() -> Result<Self> {
        let locks_dir = std::env::var("CADENCE_STUB_LOCKS")
            .map(PathBuf::from)
            .map_err(|_| Error::rejected("CADENCE_STUB_LOCKS is not set"))?;
        let command = std::env::var("CADENCE_STUB_COMMAND")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::rejected("CADENCE_STUB_COMMAND is not set"))?;
        let owned_misses = AtomicUsize::new(
            std::env::var("CADENCE_STUB_OWNED_MISS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        );
        Ok(Self {
            locks_dir,
            command,
            owned_misses,
        })
    }

    fn lock_path(&self, session: &str) -> PathBuf {
        self.locks_dir.join(format!("{session}.lock"))
    }
}

impl TuiProfile for StubProfile {
    fn name(&self) -> &'static str {
        "Stub"
    }

    fn launch_command(&self, resume: Option<&str>) -> Result<String> {
        let mut argv = self.command.clone();
        if let Some(want) = resume {
            argv.push_str(&format!(" -r {}", shlex_quote(want)));
        }
        Ok(argv)
    }

    fn exit_banner(&self) -> &'static str {
        "Stub TUI exited. This pane will close."
    }

    /// Same lock-file family as Devin's: the stub TUI flocks
    /// `<locks>/<session>.lock`, so ownership is the same /proc scan.
    fn owned_session(&self, pane_pid: u32) -> Option<String> {
        if self
            .owned_misses
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return None;
        }
        let entries = std::fs::read_dir(&self.locks_dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(session) = name.strip_suffix(".lock") else {
                continue;
            };
            if lock_holders(&entry.path())
                .iter()
                .any(|&pid| descends_from(pid, pane_pid))
            {
                return Some(session.to_string());
            }
        }
        None
    }

    fn resolve_session(&self, desired: Option<&str>, found: Option<String>) -> Result<String> {
        match (desired, found) {
            (Some(want), Some(found)) if *want == found => Ok(found),
            (Some(want), Some(found)) => Err(Error::provider(format!(
                "pane owns session '{found}', expected '{want}' — \
                 changed owner fails closed"
            ))),
            (Some(want), None) => Err(Error::provider(format!(
                "pane does not hold the stub session lock for '{want}'"
            ))),
            (None, Some(found)) => Ok(found),
            (None, None) => Err(Error::provider("pane exists but owns no stub session lock")),
        }
    }

    fn refuse_takeover(&self, session: &str) -> Result<()> {
        if let Some(foreign) = lock_holders(&self.lock_path(session)).first() {
            return Err(Error::rejected(format!(
                "stub session '{session}' is locked by another terminal \
                 (pid {foreign}); close it first — no takeover"
            )));
        }
        Ok(())
    }

    fn verify_ownership(&self, native: &str, pane_pid: u32) -> Result<()> {
        if lock_holders(&self.lock_path(native))
            .iter()
            .any(|&pid| descends_from(pid, pane_pid))
        {
            Ok(())
        } else {
            Err(Error::provider(format!(
                "pane does not hold the stub session lock for '{native}'"
            )))
        }
    }

    fn open_deadline(&self) -> Duration {
        OPEN_DEADLINE
    }

    fn analyze(&self, screen: &str, _cursor: Option<(u32, u32)>) -> Probe {
        analyze_stub(screen)
    }

    fn respond_rejection(&self) -> &'static str {
        "pty endpoints have no approval channel — answer stub \
         permission prompts in the terminal itself"
    }

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}
