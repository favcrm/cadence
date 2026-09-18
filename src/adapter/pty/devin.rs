//! The Devin TUI profile: screen signatures, the `analyze` reduction,
//! launch argv, and session ownership proven through Devin's native
//! session locks.
//!
//! Native session ownership is *proven*, not assumed: the pane's
//! process tree must hold the flock at
//! `~/.local/share/devin/cli/session_locks/<session>.lock`. A lock held
//! by a foreign process means another TUI owns the session — open
//! refuses rather than taking it over, and a changed owner fails
//! closed.

use std::path::PathBuf;
use std::time::Duration;

use crate::adapter::Probe;
use crate::error::{Error, Result};

use super::profile::TuiProfile;
use super::{descends_from, lock_holders, resolve_on_path, shlex_quote};

/// Bounded wait for the launched Devin TUI to take a native session lock.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// How much of the screen bottom counts as the status region: input
/// line, divider, status bar and a menu tall enough for Devin's
/// approval select. Approval markers only match inside it — the
/// transcript above can legitimately show these strings as text.
/// (Busy is anchored tighter still: the status row directly above the
/// input box — see `analyze_devin`.)
const STATUS_LINES: usize = 14;

/// Devin TUI screen signatures — THE one place they live. A provider
/// TUI update means editing this table, never the gate logic. Every
/// string is verbatim from the shipped binary or a live pane capture.
mod devin_screen {
    /// Glyph leading the input line (also the first option of an open
    /// approval menu — approval is checked before input parsing).
    pub const PROMPT: &str = "❭";
    /// The input line's placeholder text — presence means EMPTY input.
    pub const PLACEHOLDER: &str = "Ask Devin to build features";
    /// The input line's watermark while a turn runs — also EMPTY input.
    /// A submitted body echoes into the transcript and the input gets
    /// this placeholder; reading it as a staged draft would misjudge a
    /// healthy turn as unsubmitted (observed live with a ~3.6KB body).
    pub const BUSY_PLACEHOLDER: &str = "Guide Devin while it works";
    /// Spinner labels on the status row directly above the input box
    /// while a turn runs — `<label> · Ns (esc twice to interrupt)`.
    pub const SPINNER: &[&str] = &["Thinking", "Typing", "Running tools"];
    /// Interrupt hints on that same status row. Busy evidence only in
    /// that position: a `Did you know` tip in the region can quote the
    /// same strings and stays neutral.
    pub const INTERRUPT: &[&str] = &[
        "(esc again to interrupt)",
        "(esc twice to interrupt)",
        "Cancel agent (esc twice)",
    ];
    /// An open select/permission menu — the hint-bar fragments plus the
    /// option labels only a menu renders. `↑↓ select · ↵ confirm ·
    /// esc cancel` is the verbatim approval footer; `↓↑ to select` is
    /// the same control on the directory-trust prompt.
    pub const APPROVAL: &[&str] = &[
        "(Approve",
        " to select",
        "↑↓ select",
        "↵ confirm",
        "esc cancel",
        "Yes, switch to bypass mode",
        "No, keep",
    ];
    /// TUI-side staged queue while busy (A26 sibling: text was staged,
    /// not dropped — still not safe to add to).
    pub const QUEUED: &[&str] = &["Press Enter to send queued messages"];
}

/// Leading characters Devin's TUI interprets before the prompt text —
/// observed live in a scratch pane (CAD-32): `/` opens the slash-
/// command menu, `!` switches to bash mode, `@` opens the file-picker.
/// `#` stays a literal draft and is deliberately absent.
pub const FORBIDDEN_PREFIXES: &[char] = &['/', '!', '@'];

/// Reduce a captured Devin screen to gate facts. The last `❭` line is
/// the input line; text after it that is not the placeholder is a
/// staged draft. Menus and busy markers win over prompt parsing — a
/// `❭` leads the first approval option too. Approval menus are only
/// read in the bottom status region (the transcript above can
/// legitimately print the same strings), and busy is anchored tighter
/// still: the busy watermark in the input line, or the status row
/// directly above the box — the spinner label or interrupt hint.
/// A `Did you know` tip in the region is neutral: it quotes the same
/// hints as documentation, never as a status row. The region is
/// anchored at the last NON-BLANK row — `capture-pane` pads the
/// capture to pane height, so a young session on a tall pane has
/// blank rows below the real content.
pub fn analyze_devin(screen: &str) -> Probe {
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
    let approval_menu = devin_screen::APPROVAL.iter().any(|m| tail.contains(m));
    let lines: Vec<&str> = screen.lines().collect();
    let prompt_idx = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with(devin_screen::PROMPT));
    let prompt_visible = prompt_idx.is_some();
    let draft = prompt_idx
        .map(|i| {
            lines[i]
                .trim_start()
                .trim_start_matches(devin_screen::PROMPT)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    // Busy is decided by positive evidence tied to the input box: the
    // busy watermark in the input line itself, or the status row
    // directly above the box — the spinner label or interrupt hint.
    // Blank rows and the box's rules (which can carry embedded status
    // text like `(bypass permissions on)`) sit between the two and are
    // skipped; a `Did you know` tip in the region is neutral — it
    // quotes the same hints as documentation, never as a status row.
    let status_row = prompt_idx.and_then(|i| {
        lines[..i].iter().rev().find(|l| {
            let t = l.trim();
            !t.is_empty() && t.chars().filter(|c| matches!(c, '─' | '═')).count() < 8
        })
    });
    let status_busy = status_row.is_some_and(|row| {
        devin_screen::SPINNER.iter().any(|m| row.contains(m))
            || devin_screen::INTERRUPT.iter().any(|m| row.contains(m))
            || devin_screen::QUEUED.iter().any(|m| row.contains(m))
    });
    let input_nonempty = !draft.is_empty()
        && !draft.starts_with(devin_screen::PLACEHOLDER)
        && !draft.starts_with(devin_screen::BUSY_PLACEHOLDER);
    // Defence in depth: the busy watermark in the input line is itself
    // a busy signal, checked before the status row so the reason stays
    // precise — and so the verdict survives even if the row ever falls
    // outside the capture.
    let watermark_busy = draft.starts_with(devin_screen::BUSY_PLACEHOLDER);
    let busy_marker = status_busy || watermark_busy;
    let (idle, reason) = if approval_menu {
        (false, "approval menu is open")
    } else if watermark_busy {
        (false, "tui is busy (guide watermark in the input line)")
    } else if status_busy {
        (false, "tui is busy (status row above the input box)")
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

/// The Devin profile: `devin` CLI argv, native session locks under
/// `session_locks/`, and the Devin screen analyzer.
pub struct DevinProfile {
    /// `~/.local/share/devin/cli/session_locks` — overridable in tests.
    locks_dir: PathBuf,
    /// Absolute `devin` argv resolved at construction.
    command: String,
    /// `params.permission_mode` replayed into every launch argv —
    /// fresh and `-r` resume alike. `None` keeps Devin's own default.
    permission_mode: Option<String>,
}

impl DevinProfile {
    /// Resolve the profile from the environment: the locks dir
    /// (`CADENCE_DEVIN_LOCKS`, else the real `session_locks`) and the
    /// launch command (`CADENCE_DEVIN_COMMAND` used verbatim — tests
    /// pass `python3 mock.py <dir>` — else a `devin` found on PATH,
    /// shell-quoted for the pane shell).
    pub fn new() -> Result<Self> {
        let locks_dir = std::env::var("CADENCE_DEVIN_LOCKS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(".local/share/devin/cli/session_locks")
            });
        let command = match std::env::var("CADENCE_DEVIN_COMMAND") {
            Ok(cmd) if !cmd.is_empty() => cmd,
            _ => resolve_on_path("devin").map(|p| shlex_quote(&p))?,
        };
        Ok(Self {
            locks_dir,
            command,
            permission_mode: None,
        })
    }

    /// Set the launch permission mode from the agent's stored params —
    /// replayed into every launch argv, fresh and `-r` resume alike.
    pub fn with_permission_mode(mut self, mode: Option<String>) -> Self {
        self.permission_mode = mode;
        self
    }

    fn lock_path(&self, session: &str) -> PathBuf {
        self.locks_dir.join(format!("{session}.lock"))
    }
}

impl TuiProfile for DevinProfile {
    fn name(&self) -> &'static str {
        "Devin"
    }

    fn launch_command(&self, resume: Option<&str>) -> Result<String> {
        let mut argv = self.command.clone();
        // `--permission-mode` is a top-level flag — it combines with
        // `-r` on resume exactly as on a fresh launch.
        if let Some(mode) = &self.permission_mode {
            argv.push_str(&format!(" --permission-mode {}", shlex_quote(mode)));
        }
        if let Some(want) = resume {
            argv.push_str(&format!(" -r {}", shlex_quote(want)));
        }
        Ok(argv)
    }

    fn exit_banner(&self) -> &'static str {
        "Devin exited. This pane will close."
    }

    /// The native session whose lock is held by a descendant of
    /// `pane_pid`, or `None` when the pane owns no session.
    fn owned_session(&self, pane_pid: u32) -> Option<String> {
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

    /// Adopt the pane's owned session, refusing a mismatch with the
    /// wanted one — a changed owner fails closed, never adopts.
    fn resolve_session(&self, desired: Option<&str>, found: Option<String>) -> Result<String> {
        match (desired, found) {
            (Some(want), Some(found)) if *want == found => Ok(found),
            (Some(want), Some(found)) => Err(Error::provider(format!(
                "pane owns session '{found}', expected '{want}' — \
                 changed owner fails closed"
            ))),
            (Some(want), None) => Err(Error::provider(format!(
                "pane does not hold the Devin session lock for '{want}'"
            ))),
            (None, Some(found)) => Ok(found),
            (None, None) => Err(Error::provider(
                "pane exists but owns no Devin session lock",
            )),
        }
    }

    /// Refuse takeover: a lock held outside our (future) pane means
    /// another TUI already owns the native session.
    fn refuse_takeover(&self, session: &str) -> Result<()> {
        if let Some(foreign) = lock_holders(&self.lock_path(session)).first() {
            return Err(Error::rejected(format!(
                "Devin session '{session}' is locked by another terminal \
                 (pid {foreign}); close it first — no takeover"
            )));
        }
        Ok(())
    }

    /// The pane owns `native` while a descendant of its pid holds the
    /// session lock.
    fn verify_ownership(&self, native: &str, pane_pid: u32) -> Result<()> {
        if lock_holders(&self.lock_path(native))
            .iter()
            .any(|&pid| descends_from(pid, pane_pid))
        {
            Ok(())
        } else {
            Err(Error::provider(format!(
                "pane does not hold the Devin session lock for '{native}'"
            )))
        }
    }

    fn open_deadline(&self) -> Duration {
        OPEN_DEADLINE
    }

    fn analyze(&self, screen: &str, _cursor: Option<(u32, u32)>) -> Probe {
        analyze_devin(screen)
    }

    fn respond_rejection(&self) -> &'static str {
        "pty endpoints have no approval channel — answer Devin \
         permission prompts in the terminal itself"
    }

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_devin, DevinProfile, STATUS_LINES};
    use crate::adapter::pty::profile::TuiProfile;
    use crate::adapter::registry::DEVIN_PERMISSION_MODES;
    use std::path::PathBuf;

    fn profile(mode: Option<&str>) -> DevinProfile {
        DevinProfile {
            locks_dir: PathBuf::from("/nonexistent"),
            command: "devin".to_string(),
            permission_mode: mode.map(str::to_string),
        }
    }

    #[test]
    fn launch_argv_replays_permission_mode_fresh_and_resume() {
        for mode in DEVIN_PERMISSION_MODES {
            let p = profile(Some(mode));
            assert_eq!(
                p.launch_command(None).unwrap(),
                format!("devin --permission-mode '{mode}'")
            );
            assert_eq!(
                p.launch_command(Some("slug-1")).unwrap(),
                format!("devin --permission-mode '{mode}' -r 'slug-1'")
            );
        }
    }

    #[test]
    fn launch_argv_omits_flag_when_mode_unset() {
        let p = profile(None);
        assert_eq!(p.launch_command(None).unwrap(), "devin");
        assert_eq!(
            p.launch_command(Some("slug-1")).unwrap(),
            "devin -r 'slug-1'"
        );
    }

    #[test]
    fn permission_mode_is_shell_quoted() {
        // Values are validated upstream, but a hand-edited params row
        // must never reach the pane shell unquoted.
        let p = profile(Some("dangerous; rm -rf /"));
        let argv = p.launch_command(None).unwrap();
        assert_eq!(argv, "devin --permission-mode 'dangerous; rm -rf /'");
    }

    /// Real idle screen captured from a live Devin pane.
    const IDLE: &str = "\
❭ Ask Devin to build features, fix bugs, or work on your code
──────────────────────────────────────────────────────────────────
SWE-2 Max                                          Context: 43k / 262k";

    /// The input box's horizontal rule — can carry embedded status
    /// text (`(bypass permissions on)`).
    const RULE: &str = "──────────────────────────────────────────────────────────────────";

    /// Idle pane with a `Did you know` tip banner between the
    /// transcript and the input box — captured verbatim from a live
    /// pane (CAD-50). The tip body quotes the same Ctrl+O hint the
    /// busy status row shows; it must stay neutral.
    const IDLE_TIP: &str = "\
transcript tail

 ✱ Did you know
   Press Ctrl+O to view the full thinking trace

──────────────────────────────────────────────────────────────────
❭ Ask Devin to build features, fix bugs, or work on your code
──────────────────────────────────────────────────────────────────
SWE-2 Max                                          Context: 43k / 262k";

    /// Busy pane with the same tip banner still visible — the status
    /// row directly above the box is the busy evidence (captured
    /// shape; the rule carries `(bypass permissions on)`).
    const BUSY_TIP: &str = "\
 ✱ Did you know
   Press Ctrl+O to view the full thinking trace
⡆⠀ Running tools · 5m 51s (esc twice to interrupt)
─────────────── (bypass permissions on) ────────────────────────────
❭ Guide Devin while it works
───────────────────────────────────────────────────────────────────
SWE-2 Max                          Context: 153k / 262k tokens (58%)";

    /// Real approval menu captured verbatim from a live Devin pane
    /// (v3000.10.31): option list plus the `↑↓ select · ↵ confirm ·
    /// esc cancel` footer.
    const APPROVAL: &str = "\
Allow this tool call?
❭ 1 Yes  (Approve once)
· 2 Yes, allow `env` commands
· 3 Yes, always allow `env` commands in `cadence-smoke-repo`
· 4 Yes, always allow `env` commands in all projects
· 5 Yes, switch to bypass mode
· 6 Edit command
· 7 Describe change to command
· 8 No
↑↓ select · ↵ confirm · esc cancel";

    #[test]
    fn idle_prompt_is_pasteable() {
        let p = analyze_devin(IDLE);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(p.prompt_visible && !p.input_nonempty);
        assert!(!p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn approval_menu_wins_over_prompt_shape() {
        // The menu's first option also leads with `❭` — menu detection
        // must outrank prompt parsing or it reads as a draft.
        let p = analyze_devin(APPROVAL);
        assert!(!p.idle);
        assert!(p.approval_menu);
        assert_eq!(p.reason, "approval menu is open");
    }

    #[test]
    fn status_row_markers_block_even_with_prompt() {
        // Busy evidence lives on the status row directly above the
        // input box — spinner labels, interrupt hints, the staged-
        // queue hint. The same strings elsewhere stay neutral.
        for row in [
            "⠸  Thinking · 0s (esc twice to interrupt)",
            "⡆⠀ Running tools · 5m 51s (esc twice to interrupt)",
            "Typing · 2s (esc again to interrupt)",
            "Cancel agent (esc twice)",
            "Press Enter to send queued messages",
        ] {
            let screen = format!("{row}\n{RULE}\n❭ Guide Devin while it works\n{RULE}\nSWE-2 Max");
            let p = analyze_devin(&screen);
            assert!(!p.idle && p.busy_marker, "{row}: {}", p.reason);
        }
    }

    #[test]
    fn idle_tip_banner_is_neutral_not_busy() {
        // CAD-50: the `Did you know` tip quotes the Ctrl+O hint the
        // busy status row shows — a tip between transcript and box is
        // neutral, never busy evidence.
        let p = analyze_devin(IDLE_TIP);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(p.prompt_visible && !p.input_nonempty);
        assert!(!p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn busy_with_tip_still_blocks() {
        // The tip stays neutral but the status row directly above the
        // box is authoritative — a really busy pane still reads busy.
        // The watermark on the input line reports first.
        let p = analyze_devin(BUSY_TIP);
        assert!(!p.idle && p.busy_marker, "{} / {}", p.idle, p.reason);
        assert_eq!(p.reason, "tui is busy (guide watermark in the input line)");
    }

    #[test]
    fn status_row_above_box_reports_busy_reason() {
        // A spinner row with the idle placeholder still in the input
        // (turn just started): the status row is the busy evidence.
        let screen = format!(
            "⠸  Thinking · 2s (esc twice to interrupt)\n{RULE}\n❭ Ask Devin to build features, fix bugs, or work on your code\n{RULE}\nSWE-2 Max"
        );
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.busy_marker);
        assert_eq!(p.reason, "tui is busy (status row above the input box)");
    }

    #[test]
    fn markers_elsewhere_in_region_do_not_count() {
        // A marker quoted in the region but NOT on the status row is
        // transcript-like content — only the row directly above the
        // box is authoritative.
        let screen = format!(
            "note: (esc twice to interrupt) appeared in output\nordinary row\n{RULE}\n❭ Ask Devin to build features, fix bugs, or work on your code\n{RULE}\nSWE-2 Max"
        );
        let p = analyze_devin(&screen);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(!p.busy_marker);
    }

    #[test]
    fn busy_watermark_is_empty_input_not_a_draft() {
        // Observed live (F7): after a ~3.6KB body submits, the text
        // echoes into the transcript and the input line shows the busy
        // watermark `❭ Guide Devin while it works` — an EMPTY input,
        // not a staged draft. Reading it as a draft made the render
        // check miss a healthy turn and fence a working pane.
        let busy = "\
Sentence 37 of the long-body observation fills the input line. TAILMARKER-9Z7X-END
⠸  Thinking · 0s (esc twice to interrupt)
──────────────────────────────────────────────────────────────────
❭ Guide Devin while it works
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let p = analyze_devin(busy);
        assert!(!p.idle && p.busy_marker, "{}", p.reason);
        assert!(!p.input_nonempty, "busy watermark must read as empty");
    }

    #[test]
    fn staged_tui_queue_is_busy_not_idle() {
        // A26 sibling: the TUI shows queued input while busy — adding
        // more to it is still unsafe. The hint renders on the status
        // row directly above the box.
        let screen = format!(
            "Press Enter to send queued messages\n{RULE}\n❭ Guide Devin while it works\n{RULE}\nSWE-2 Max"
        );
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.busy_marker);
    }

    #[test]
    fn typed_draft_is_not_idle() {
        let screen = IDLE.replacen(
            "Ask Devin to build features, fix bugs, or work on your code",
            "half-typed human draft",
            1,
        );
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.input_nonempty);
        assert_eq!(p.reason, "unsubmitted text in the input line");
    }

    #[test]
    fn no_prompt_line_is_not_idle() {
        let p = analyze_devin("compiling…\nsome output without a prompt");
        assert!(!p.idle && !p.prompt_visible);
        assert_eq!(p.reason, "no prompt line visible");
    }

    #[test]
    fn markers_in_transcript_do_not_count_as_busy() {
        // The transcript may legitimately print the marker strings
        // (this repo's own source does). Only the bottom status region
        // is authoritative — a marker scrolled above it must not stall
        // an idle pane.
        let mut screen = String::new();
        for marker in [
            "(esc twice to interrupt)",
            "Guide Devin while it works",
            "↑↓ select · ↵ confirm · esc cancel",
            "Press Enter to send queued messages",
        ] {
            screen.push_str(&format!("transcript line: {marker}\n"));
        }
        for i in 0..STATUS_LINES {
            screen.push_str(&format!("ordinary output row {i}\n"));
        }
        screen.push_str(IDLE);
        // Same verdict with and without capture-pane's blank padding.
        for blanks in [
            "",
            "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n",
        ] {
            let padded = format!("{screen}{blanks}");
            let p = analyze_devin(&padded);
            assert!(p.idle, "{} / {}", p.idle, p.reason);
            assert!(!p.busy_marker && !p.approval_menu);
        }
    }

    #[test]
    fn markers_on_the_status_row_still_block() {
        // The spinner row directly above the box is the live status —
        // it blocks even when the rest of the region looks calm.
        let screen = format!(
            "⠀⠇ Thinking · 30s (esc twice to interrupt)\n{RULE}\n❭ Ask Devin to build features, fix bugs, or work on your code\n{RULE}\nSWE-2 Max"
        );
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.busy_marker);
    }

    #[test]
    fn trailing_blank_rows_do_not_hide_busy() {
        // capture-pane pads to pane height: a young session on a tall
        // pane leaves blank rows below the real content. The status
        // region must anchor at the last non-blank row — otherwise a
        // busy pane reads idle and the daemon pastes into it.
        let busy = "\
⠸  Thinking · 12s (esc twice to interrupt)
──────────────────────────────────────────────────────────────────
❭ Guide Devin while it works
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let screen = format!("{busy}{}", "\n".repeat(30));
        let p = analyze_devin(&screen);
        assert!(!p.idle, "busy pane with trailing blanks read idle");
        assert!(p.busy_marker, "{:?}", p);
    }

    #[test]
    fn trailing_blank_rows_do_not_hide_approval() {
        let screen = format!("{APPROVAL}{}", "\n".repeat(30));
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.approval_menu, "{:?}", p);
        assert_eq!(p.reason, "approval menu is open");
    }

    #[test]
    fn busy_watermark_alone_is_not_idle() {
        // Defence in depth: the guide watermark in the input line is a
        // busy signal even when no other marker survives the region
        // maths (e.g. the banner scrolled just above the window).
        let screen = "\
some transcript output
❭ Guide Devin while it works
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let p = analyze_devin(screen);
        assert!(!p.idle && p.busy_marker);
        assert_eq!(p.reason, "tui is busy (guide watermark in the input line)");
        assert!(!p.input_nonempty);
    }
}
