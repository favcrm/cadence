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

use crate::adapter::{Probe, ProviderEnv};
use crate::error::{Error, Result};

use super::profile::TuiProfile;
use super::{descends_from, lock_holders, resolve_on_path, shlex_quote};

/// Bounded wait for the launched Devin TUI to take a native session lock.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// How much of the screen bottom counts as the menu region: a
/// numbered approval menu (~12 rows) plus the busy input box and
/// status bar that can stay visible below it, so the menu's own
/// chrome sits up to ~18 rows above the frame's end. Only the
/// menu-exclusive anchors below match inside it — transcript text
/// quoting a lone option label or legend fragment never counts.
/// (Busy is anchored tighter still: the status row directly above the
/// input box — see `analyze_devin`.)
const MENU_LINES: usize = 24;

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
    /// An open select/permission menu — the anchors below are
    /// menu-exclusive and a single match decides alone: the
    /// selection footer's legend, which only ever renders on a menu
    /// (`↑↓ select · ↵ confirm · esc cancel` verbatim on the approval
    /// select, `↓↑ to select` on the directory-trust prompt). Anchors
    /// match on the trimmed row's leading glyph, so a transcript row
    /// quoting `↵ confirm` mid-sentence stays inert.
    pub const ANCHOR: &[&str] = &["↑↓ select", "↓↑ to select", "↵ confirm"];
    /// Option labels and lone legend fragments — quotable inside a
    /// long transcript, so they only count as a cluster alongside real
    /// menu structure (a second hint, or numbered option rows).
    pub const HINT: &[&str] = &[
        "(Approve",
        " to select",
        "esc cancel",
        "Yes, switch to bypass mode",
        "No, keep",
        "always allow",
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

/// A numbered menu option row: `· 3 Yes, …` or the `❭`-led selected
/// row. Returns the printed number so `approval_answer` can validate a
/// choice against the rows actually on screen.
fn option_line(line: &str) -> Option<u32> {
    let t = line.trim_start();
    let t = t
        .strip_prefix('·')
        .or_else(|| t.strip_prefix('❭'))?
        .trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !t[digits.len()..].starts_with(char::is_whitespace) {
        return None;
    }
    digits.parse().ok()
}

/// A footer/legend or option-label row matching hint `h` — anchored
/// on a legend glyph (`↑↓ select`, `↵ confirm`), a `·`-separated
/// legend cell (`· esc cancel`), an option row, or the trimmed row's
/// leading text — so a transcript quoting the same words mid-line
/// does not count.
fn hint_row(l: &str, h: &str) -> bool {
    if !l.contains(h) {
        return false;
    }
    let t = l.trim_start();
    t.starts_with(h)
        || t.contains(&format!("· {h}"))
        || option_line(l).is_some()
        || t.starts_with('↑')
        || t.starts_with('↓')
        || t.starts_with('↵')
}

/// A row carrying menu structure — a numbered or unnumbered option, a
/// `·`/`❭`-led select row, or an anchored legend/hint row. The menu's
/// own subject line (`Allow this tool call?`) is deliberately NOT one:
/// it is ordinary free text directly above the block.
fn menu_row(l: &str) -> bool {
    let t = l.trim_start();
    option_line(l).is_some()
        || t.starts_with('·')
        || t.starts_with('❭')
        || devin_screen::ANCHOR.iter().any(|a| t.starts_with(a))
        || devin_screen::HINT.iter().any(|h| hint_row(l, h))
}

/// A row that can belong to the live bottom frame — menu rows, the
/// box's `─`/`═` rules, the busy status row, a `Did you know`/`Tip:`
/// banner row, or transparent blank padding. Anything else is free
/// text: transcript output, `⏺`/`└` tool echoes, the menu subject.
fn frame_row(l: &str) -> bool {
    let t = l.trim_start();
    t.is_empty()
        || menu_row(l)
        || t.chars().filter(|c| matches!(c, '─' | '═')).count() >= 8
        || devin_screen::SPINNER.iter().any(|m| l.contains(m))
        || devin_screen::INTERRUPT.iter().any(|m| l.contains(m))
        || devin_screen::QUEUED.iter().any(|m| l.contains(m))
        || t.starts_with('✱')
}

/// The input line at row `i`: a `❭`-led row that is not a numbered
/// option, whose next non-blank row is never menu chrome — a real
/// select's `❭` highlight sits on option/legend rows, while the input
/// row sits on the box's bottom rule, the model bar, or nothing.
fn input_row(lines: &[&str], i: usize) -> bool {
    let t = lines[i].trim_start();
    if !t.starts_with(devin_screen::PROMPT) || option_line(lines[i]).is_some() {
        return false;
    }
    !lines[i + 1..]
        .iter()
        .find(|n| !n.trim().is_empty())
        .is_some_and(|n| menu_row(n))
}

/// The live menu region is the contiguous run of frame rows walking up
/// from the bottom — and it ends at the first free-text row. A real
/// permission menu renders inside the busy frame (option block, legend,
/// spinner, the `Guide Devin` box) with nothing but chrome between it
/// and the input row; a transcript quoting a menu verbatim sits above
/// the cut whenever the agent's own prose or an editable input row
/// intervenes. That position is the corroboration the legend text
/// cannot supply — a quoted `↑↓ select` is transcript text too, more
/// specific but not more trustworthy.
///
/// Residual: a quote that adjoins the busy frame verbatim — the
/// legend as the agent's last printed row, directly against the live
/// spinner and watermark box — is textually identical to a real menu
/// and still passes; closing that needs signal outside the screen
/// text (cursor position, provider state).
/// An EDITABLE input row — empty, placeholder, or a staged draft,
/// anything but the busy watermark — vetoes the region outright: a
/// real approval menu is modal and only ever renders above the busy
/// box, so menu-looking rows above a live editable `❭` are a quote,
/// and `answer` would key the choice into the draft.
fn devin_window<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let input = (0..lines.len()).rev().find(|i| input_row(lines, *i));
    match input {
        Some(i) => {
            let draft = lines[i]
                .trim_start()
                .trim_start_matches(devin_screen::PROMPT)
                .trim();
            if !draft.starts_with(devin_screen::BUSY_PLACEHOLDER) {
                // Editable input: no live menu can exist above it.
                return &lines[lines.len()..];
            }
            let mut start = i;
            while start > 0 && frame_row(lines[start - 1]) {
                start -= 1;
            }
            &lines[start..i]
        }
        // No input row on screen (a bare menu fragment): contiguous
        // frame rows up from the last row.
        None => {
            let mut start = lines.len();
            while start > 0 && frame_row(lines[start - 1]) {
                start -= 1;
            }
            &lines[start..]
        }
    }
}

/// The line naming what the menu asks: the last non-blank row above
/// the option list — a `└`-led command detail (`$ printenv FOO`) or a
/// bare header (`Allow this tool call?`).
fn menu_subject(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.trim_end().lines().collect();
    let last_opt = lines.iter().rposition(|l| option_line(l).is_some())?;
    let mut first_opt = last_opt;
    while first_opt > 0 && option_line(lines[first_opt - 1]).is_some() {
        first_opt -= 1;
    }
    lines[..first_opt]
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.trim()
                .trim_start_matches('└')
                .trim_start_matches('⏺')
                .trim()
                .chars()
                .take(100)
                .collect()
        })
}

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
    // The menu region is wider than the busy anchor: a numbered
    // approval menu can sit ABOVE a still-visible busy input box, so
    // its option rows and footer land up to ~18 rows above the frame
    // end. devin_window first cuts the region to the contiguous frame
    // chrome walking up from the input row — transcript quoting a menu
    // verbatim is separated from the live frame by the agent's own
    // prose, and an editable input row vetoes the region outright.
    // Anchors (the selection footer's legend, matched on the
    // trimmed row's leading glyph) decide alone inside the window;
    // hints (option labels, lone legend fragments) need real menu
    // structure beside them — numbered option rows or a second hint —
    // because transcript text can legitimately quote one.
    let menu_lines: Vec<&str> = content
        .lines()
        .rev()
        .take(MENU_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    // Only the contiguous frame chrome walking up from the input row
    // can be menu structure — a transcript quoting a menu verbatim
    // leaves the agent's prose between the quote and the live frame,
    // and an editable `❭` input row vetoes everything above it. The
    // quoted legend is transcript text too: more specific, not more
    // trustworthy.
    let menu_lines: &[&str] = devin_window(&menu_lines);
    let footer = menu_lines.iter().any(|l| {
        let t = l.trim_start();
        devin_screen::ANCHOR.iter().any(|a| t.starts_with(a))
    });
    let options = menu_lines
        .iter()
        .filter(|l| option_line(l).is_some())
        .count();
    let hints = devin_screen::HINT
        .iter()
        .filter(|h| menu_lines.iter().any(|l| hint_row(l, h)))
        .count();
    let approval_menu = footer || (options >= 2 && hints >= 1) || hints >= 2;
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
        (
            false,
            menu_subject(content).unwrap_or_else(|| "approval menu is open".to_string()),
        )
    } else if watermark_busy {
        (
            false,
            "tui is busy (guide watermark in the input line)".to_string(),
        )
    } else if status_busy {
        (
            false,
            "tui is busy (status row above the input box)".to_string(),
        )
    } else if !prompt_visible {
        (false, "no prompt line visible".to_string())
    } else if input_nonempty {
        (false, "unsubmitted text in the input line".to_string())
    } else {
        (true, "idle".to_string())
    };
    Probe {
        idle,
        reason,
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
    pub fn new(env: &ProviderEnv) -> Result<Self> {
        let locks_dir = env
            .var("CADENCE_DEVIN_LOCKS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(".local/share/devin/cli/session_locks")
            });
        let command = match env.var("CADENCE_DEVIN_COMMAND") {
            Some(cmd) if !cmd.is_empty() => cmd,
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

    /// Numbered menus take the option's digit key (verified live:
    /// `8` dismisses the permission select as `No` in one keystroke);
    /// an unnumbered select falls back to arrows + Enter. The option
    /// scan sees only the live frame region — `devin_window` cuts
    /// quoted menu text sitting above the real frame, so a transcript
    /// can never make this emit a keystroke into the input line.
    fn approval_answer(&self, screen: &str, choice: &str) -> Result<Vec<String>> {
        let region: Vec<&str> = screen
            .trim_end()
            .lines()
            .rev()
            .take(MENU_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let region: &[&str] = devin_window(&region);
        let options: Vec<u32> = region.iter().filter_map(|l| option_line(l)).collect();
        let n: u32 = choice.parse().map_err(|_| {
            Error::rejected(format!(
                "'{choice}' is not a menu index — Devin menus take the \
                 option's printed number"
            ))
        })?;
        if n == 0 {
            return Err(Error::rejected("menu indices start at 1"));
        }
        if options.is_empty() {
            // Unnumbered select: the option block is the contiguous
            // run of `·`/`❭`-led rows around the `❭` highlight — up
            // and down, never just the suffix below it, and never a
            // blind count when no highlight row is on screen. `region`
            // is already the live frame window.
            let marker = |l: &&str| {
                let t = l.trim_start();
                t.starts_with('·') || t.starts_with('❭')
            };
            // The highlight row must have option rows or a legend
            // right below it — the input box's `❭` is followed by the
            // box's rules, not menu rows.
            let sel = region
                .iter()
                .enumerate()
                .rev()
                .find(|(i, l)| {
                    l.trim_start().starts_with('❭')
                        && region[i + 1..]
                            .iter()
                            .find(|n| !n.trim().is_empty())
                            .is_some_and(|n| {
                                marker(n) || devin_screen::HINT.iter().any(|h| hint_row(n, h)) || {
                                    let t = n.trim_start();
                                    devin_screen::ANCHOR.iter().any(|a| t.starts_with(a))
                                }
                            })
                })
                .map(|(i, _)| i)
                .ok_or_else(|| {
                    Error::rejected(
                        "cannot locate the option rows on this menu — \
                         answer it in the pane",
                    )
                })?;
            let mut start = sel;
            while start > 0 && marker(&region[start - 1]) {
                start -= 1;
            }
            let mut end = sel;
            while end + 1 < region.len() && marker(&region[end + 1]) {
                end += 1;
            }
            let count = (end - start + 1) as u32;
            // A lone `❭` row is the input box, not a one-option menu —
            // its next non-blank row being a legend made it look like
            // a sel row, but a real menu always lists a `·` sibling.
            if count < 2 {
                return Err(Error::rejected(
                    "cannot locate the option rows on this menu — \
                     answer it in the pane",
                ));
            }
            if n > count {
                return Err(Error::rejected(format!(
                    "no option {n} on this menu — it lists {count}"
                )));
            }
            let want = (n - 1) as usize;
            let cur = sel - start;
            let (dir, steps) = if want >= cur {
                ("Down", want - cur)
            } else {
                ("Up", cur - want)
            };
            let mut keys = vec![dir.to_string(); steps];
            keys.push("Enter".to_string());
            return Ok(keys);
        }
        if !options.contains(&n) {
            return Err(Error::rejected(format!(
                "no option {n} on this menu — it lists {}",
                options
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // A single digit is one keypress — the menu selects on it
        // outright. A multi-digit index must never reach tmux as one
        // literal: `send-keys "10"` presses `1` then `0`, and the
        // first press alone would pick option 1 while `0` lands as
        // stray input. Navigate from the highlighted row instead.
        if n < 10 {
            return Ok(vec![n.to_string()]);
        }
        let region: Vec<&str> = screen
            .trim_end()
            .lines()
            .rev()
            .take(MENU_LINES)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let cur = region
            .iter()
            .rev()
            .find(|l| l.trim_start().starts_with('❭'))
            .and_then(|l| option_line(l))
            .filter(|c| options.contains(c))
            .ok_or_else(|| {
                Error::rejected(
                    "cannot locate the highlighted option on this menu — \
                     answer it in the pane",
                )
            })?;
        let want = options.iter().position(|o| *o == n).unwrap();
        let at = options.iter().position(|o| *o == cur).unwrap();
        let (dir, steps) = if want >= at {
            ("Down", want - at)
        } else {
            ("Up", at - want)
        };
        let mut keys = vec![dir.to_string(); steps];
        keys.push("Enter".to_string());
        Ok(keys)
    }

    /// Devin interrupts on a second Esc (`esc twice to interrupt`).
    fn interrupt_keys(&self) -> &'static [&'static str] {
        &["Escape", "Escape"]
    }

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_devin, DevinProfile, MENU_LINES};
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

    /// A numbered menu with ten options — the highlight sits on
    /// `❭ 1`, and option 10 must be arrowed to: `send-keys "10"`
    /// would press `1` (selecting it outright) then leak `0`.
    const LONG_MENU: &str = "\
Allow this tool call?
❭ 1 Yes
· 2 B
· 3 C
· 4 D
· 5 E
· 6 F
· 7 G
· 8 H
· 9 I
· 10 No
↑↓ select · ↵ confirm · esc cancel";

    #[test]
    fn multi_digit_answer_navigates_instead_of_typing() {
        let prof = profile(None);
        // Option 10 is nine rows below the highlighted option 1.
        assert_eq!(
            prof.approval_answer(LONG_MENU, "10").unwrap(),
            vec!["Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Enter"]
        );
        // A single digit still takes its key.
        assert_eq!(prof.approval_answer(LONG_MENU, "8").unwrap(), vec!["8"]);
        // Beyond the printed list still refuses.
        assert!(prof.approval_answer(LONG_MENU, "11").is_err());
    }

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
        // The reason names what the menu asks — the line above the
        // option list.
        assert_eq!(p.reason, "Allow this tool call?");
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
        for i in 0..MENU_LINES {
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
        assert_eq!(p.reason, "Allow this tool call?");
    }

    #[test]
    fn approval_menu_above_the_busy_box_is_still_a_menu() {
        // CAD-102: the incident layout — the numbered menu renders
        // ABOVE a still-visible busy input box, so its option rows and
        // footer land ~18 rows above the frame end, outside the busy
        // anchor. Reading it as "busy (guide watermark)" is the bug:
        // a menu is not ordinary busy — it needs an operator answer.
        let screen = "\
❭ run the shell command: printenv FOO
 ⏺ Running command
 └ $ printenv FOO

❭ 1 Yes  (Approve once)
· 2 Yes, allow `printenv` commands
· 3 Yes, always allow `printenv` commands in `tmp`
· 4 Yes, always allow `printenv` commands in all projects
· 5 Yes, switch to bypass mode
· 6 Edit command
· 7 Describe change to command
· 8 No
↑↓ select · ↵ confirm · esc cancel

──────────────────────────────────────────────────────────────────
❭ Guide Devin while it works
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let p = analyze_devin(screen);
        assert!(!p.idle && p.approval_menu, "{:?}", p);
        // The menu wins over the watermark busy below it — and the
        // reason names the command being approved, not generic busy.
        assert_eq!(p.reason, "$ printenv FOO");
    }

    #[test]
    fn numbered_options_need_menu_structure() {
        // Transcript text quoting a lone option label or legend word
        // stays inert — hints only count beside real menu structure.
        let screen = "\
earlier the menu offered `No, keep` as the last choice
❭ Ask Devin to build features
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let p = analyze_devin(screen);
        assert!(p.idle && !p.approval_menu, "{:?}", p);
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

    /// An unnumbered select (`↓↑ to select` legend, `❭`-led
    /// highlight, `·`-led options) for the answer tests.
    fn unnumbered_menu() -> &'static str {
        " Trust this directory?\n❭ Yes, trust it\n· No, keep asking\n↑↓ select · ↵ confirm · esc cancel\n"
    }

    #[test]
    fn answer_on_unnumbered_menu_navigates_the_block() {
        let prof = profile(None);
        // The highlight sits on option 1 — Down once selects the
        // second printed option.
        assert_eq!(
            prof.approval_answer(unnumbered_menu(), "2").unwrap(),
            vec!["Down", "Enter"]
        );
        assert_eq!(
            prof.approval_answer(unnumbered_menu(), "1").unwrap(),
            vec!["Enter"]
        );
    }

    #[test]
    fn answer_counts_options_above_the_highlight() {
        // The highlight is on the SECOND printed option — option 1 is
        // the row above it, so `answer 1` moves Up, never Enter.
        let screen = " Trust this directory?\n· Yes, trust it\n❭ No, keep asking\n↑↓ select · ↵ confirm · esc cancel\n";
        let prof = profile(None);
        assert_eq!(
            prof.approval_answer(screen, "1").unwrap(),
            vec!["Up", "Enter"]
        );
        assert_eq!(prof.approval_answer(screen, "2").unwrap(), vec!["Enter"]);
    }

    #[test]
    fn answer_index_is_bounded_by_the_visible_block() {
        let prof = profile(None);
        // A huge index must never reach key allocation — the menu
        // lists two options, so anything past 2 refuses.
        for choice in ["3", "4000000000", "0"] {
            assert!(
                prof.approval_answer(unnumbered_menu(), choice).is_err(),
                "{choice} must refuse"
            );
        }
        // And a menu shape with no option block at all refuses
        // outright — never a blind arrow walk.
        let footer_only = "transcript\n↑↓ select · ↵ confirm · esc cancel\n";
        assert!(
            prof.approval_answer(footer_only, "1").is_err(),
            "unparseable options must refuse"
        );
        assert!(prof.approval_answer(footer_only, "4000000000").is_err());
    }

    #[test]
    fn quoted_legend_text_is_not_a_menu() {
        // The same words mid-line in a transcript stay inert — only
        // glyph-anchored legend rows and option rows count.
        let screen = "\
docs say \"esc cancel\" dismisses the prompt and that you can always allow tools
❭ Ask Devin to build features
──────────────────────────────────────────────────────────────────
SWE-2 Max                                      Alt+Enter for multiline prompts";
        let p = analyze_devin(screen);
        assert!(p.idle && !p.approval_menu, "{:?}", p);
    }

    /// CAD-102 r6: a transcript that quotes a real menu verbatim —
    /// anchor, numbered options, the selection legend — above a live
    /// IDLE input box. The quoted legend is transcript text, so the
    /// frame decides instead: the agent's own prose sits between the
    /// quote and the live `❭` box, and an editable input row vetoes
    /// menu structure outright. `answer` must refuse — the keystroke
    /// is the consequence that matters, not the probe flag.
    #[test]
    fn devin_quoted_menu_above_idle_box_is_inert() {
        let screen = "\
● The pane showed:

  Allow this tool call?
  ❭ 1 Yes  (Approve once)
  · 2 Yes, allow `env` commands
  · 8 No
  ↑↓ select · ↵ confirm · esc cancel

  So it is waiting.

──────────────────────────────────────────────────────────────────
❭ Ask Devin to build features, fix bugs, or work on your code
──────────────────────────────────────────────────────────────────
SWE-2 Max                                          Context: 43k / 262k";
        let p = analyze_devin(screen);
        assert!(!p.approval_menu, "{p:?}");
        assert!(p.idle, "{p:?}");
        let prof = profile(None);
        assert!(prof.approval_answer(screen, "2").is_err());
        assert!(prof.approval_answer(screen, "8").is_err());
        // Even flush against the box — no intervening prose — the
        // editable `❭` vetoes it.
        let flush = "\
Allow this tool call?
❭ 1 Yes  (Approve once)
· 8 No
↑↓ select · ↵ confirm · esc cancel
──────────────────────────────────────────────────────────────────
❭ half-typed draft
──────────────────────────────────────────────────────────────────
SWE-2 Max                                          Context: 43k / 262k";
        let p = analyze_devin(flush);
        assert!(!p.approval_menu && p.input_nonempty, "{p:?}");
        assert!(prof.approval_answer(flush, "1").is_err());
    }

    /// The same quote above a live BUSY box: the spinner and the
    /// `Guide Devin` watermark are real, so the editable-input veto
    /// does not fire — but the agent's prose between the quote and
    /// the busy frame still cuts the window, and `answer` must refuse
    /// rather than fire the digit mid-turn.
    #[test]
    fn devin_quoted_menu_above_busy_box_is_inert() {
        let screen = "\
● The pane showed:

  Allow this tool call?
  ❭ 1 Yes  (Approve once)
  · 2 Yes, allow `env` commands
  · 8 No
  ↑↓ select · ↵ confirm · esc cancel

  So it is waiting.
⠸ Running tools · 2m 10s (esc twice to interrupt)
──────────
❭ Guide Devin while it works
";
        let p = analyze_devin(screen);
        assert!(!p.approval_menu, "{p:?}");
        assert!(p.busy_marker, "{p:?}");
        let prof = profile(None);
        assert!(prof.approval_answer(screen, "2").is_err());
    }
}
