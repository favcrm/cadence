//! The Claude Code TUI profile: screen signatures, the `analyze`
//! reduction, launch argv, and session ownership proven through
//! Claude's own per-process session registry.
//!
//! Native session ownership is *proven*, not assumed: Claude records
//! every interactive process in `~/.claude/sessions/<pid>.json` —
//! `{"pid": …, "sessionId": …, "cwd": …, "procStart": …}` — and the
//! pane's process tree must contain that pid. An entry whose pid is
//! dead or re-started (procStart mismatch) is stale and ignored; a
//! session claimed by a live foreign pid means another TUI owns it —
//! open refuses rather than taking it over, and a changed owner fails
//! closed.
//!
//! One Claude-specific rendering fact shapes the analyzer: an idle
//! input box shows dim (`ESC[2m`) "ghost" text — a prompt suggestion
//! or a `Try "…"` placeholder — that a plain-text capture cannot tell
//! from a staged draft. The adapter probes with a styled capture
//! (`capture-pane -e`), so [`analyze_claude_styled`] reads the input
//! line's undimmed text as the draft and ignores the dim cells (CAD-294).
//! A plain capture ([`analyze_claude`]) falls back to the cursor: the
//! suggestion never moves it off the prompt start, typed input does.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::adapter::{Probe, ProviderEnv};
use crate::error::{Error, Result};
use crate::store::Agent;

use super::profile::{DraftView, TuiProfile};
use super::{descends_from, resolve_on_path, shlex_quote};

/// Bounded wait for the launched Claude TUI to publish its session
/// registry entry.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// How much of the screen bottom counts as the status region: input
/// box, status bar and a menu tall enough for Claude's permission
/// select. Busy/approval markers only match inside it — the transcript
/// above can legitimately show these strings as text.
const STATUS_LINES: usize = 16;
/// The wider window a menu may occupy: a permission prompt (~10 rows)
/// plus the input box and status bar that can stay visible below it.
/// Menu anchors only match inside it — the transcript above can
/// legitimately quote the same strings.
const MENU_LINES: usize = 24;
/// How far above the input box's top border a running turn's status
/// row may sit: the row itself, a blank, and the turn's todo list or
/// queued-message rows rendered under it.
const SPINNER_REACH: usize = 12;

/// Claude TUI screen signatures — THE one place they live. A provider
/// TUI update means editing this table, never the gate logic. Every
/// string is verbatim from a live pane capture (2.1.275; the
/// workspace-trust dialog was captured on 2.1.276 — the build carries
/// both its current and older wording defensively so either still
/// reads as a menu, never as idle).
mod claude_screen {
    /// Glyph leading the input box (also the selected option of an
    /// open menu — the box is told apart by its `─` border above).
    pub const PROMPT: &str = "❯";
    /// The box's top border row — only `─` cells.
    pub const BORDER: char = '─';
    /// On-screen markers while a turn is running: the spinner line's
    /// interrupt hint and a tool call's pending marker.
    pub const BUSY: &[&str] = &["esc to interrupt", "Waiting…"];
    /// Frames of the glyph leading a turn's status row. 2.1.280 no
    /// longer prints `esc to interrupt` there — while a turn runs the
    /// row reads `✻ Warping… (1m 35s · ↓ 6.5k tokens · …)`, and once it
    /// ends `✻ Crunched for 18s · done 11:57 AM` (CAD-285).
    pub const SPINNER: &[char] = &['·', '✢', '✳', '✶', '✻', '✽', '*'];
    /// An open select/permission menu — the anchors are the permission
    /// prompt's title plus the workspace-trust dialog in both observed
    /// 2.1.x wordings. They are natural-language rows, so a match only
    /// counts beside real menu structure (a numbered option run, a
    /// `❯`-led option, or a hint legend); an indented transcript row
    /// quoting the same words stays inert. The hints are footer
    /// fragments — quotable in a long transcript, so they decide only
    /// as a cluster alongside menu structure.
    pub const ANCHOR: &[&str] = &[
        "Do you want to proceed?",
        "Quick safety check",
        "Yes, I trust this folder",
        "Do you trust the files in this folder?",
    ];
    pub const HINT: &[&str] = &["Esc to cancel", "Tab to amend", "Enter to confirm"];
}

/// Leading characters Claude's TUI interprets before the prompt text —
/// observed live in a scratch pane (CAD-17): `!` switches to shell
/// mode, `/` opens the command menu, `@` opens the agent/file picker.
/// `#` stays a literal draft and is deliberately absent.
pub const FORBIDDEN_PREFIXES: &[char] = &['/', '!', '@'];

/// A numbered menu option row: `❯ 1. Yes` or `   2. …` — returns the
/// printed number so `approval_answer` can validate a choice against
/// the rows actually on screen.
fn option_line(line: &str) -> Option<u32> {
    let t = line.trim_start().trim_start_matches('❯').trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !t[digits.len()..].starts_with('.') {
        return None;
    }
    digits.parse().ok()
}

/// The bottom-most contiguous run of `N.` rows, but only when it is
/// real menu structure: a `❯`-led option row inside the run, or the
/// anchored menu legend directly after it. A markdown numbered list
/// quoted in the transcript satisfies neither — it is never a menu's
/// options.
fn numbered_block(lines: &[&str]) -> Vec<u32> {
    // Only rows below the live input box can be menu structure —
    // a quoted menu sitting above the box is transcript text.
    let lines = menu_window(lines);
    let mut runs: Vec<Vec<(usize, u32)>> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if let Some(n) = option_line(l) {
            match runs.last_mut() {
                // A run is contiguous rows numbered sequentially — a
                // restart (`1. … 2. … 1. …`) is a new list, and only
                // the bottom-most run can be the menu's.
                Some(r) if r.last().unwrap().0 + 1 == i && n == r.last().unwrap().1 + 1 => {
                    r.push((i, n))
                }
                _ => runs.push(vec![(i, n)]),
            }
        }
    }
    let Some(run) = runs.last() else {
        return Vec::new();
    };
    let end = run.last().unwrap().0;
    // The `❯` must be a menu's indented highlight — a column-0 `❯`
    // is a transcript echo of the operator's own input, never a menu
    // option.
    let has_selected = run.iter().any(|(i, _)| sel_row(lines, *i));
    let legend_after = lines[end + 1..]
        .iter()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| claude_screen::HINT.iter().any(|h| hint_row(l, h)));
    if has_selected || legend_after {
        run.iter().map(|(_, n)| *n).collect()
    } else {
        Vec::new()
    }
}

/// A footer/legend row matching hint `h` — anchored on the trimmed
/// row's leading text or a `·`-separated legend cell (`Enter to
/// confirm · Esc to cancel`), so a transcript quoting the same words
/// mid-line does not count.
fn hint_row(l: &str, h: &str) -> bool {
    l.contains(h) && (l.trim_start().starts_with(h) || l.contains(&format!("· {h}")))
}

/// True when row `i` is the boxed input line: a `❯`-led row whose
/// previous row is the `─` border. Menu option lists lead with `❯`
/// too but are never boxed.
fn boxed(lines: &[&str], i: usize) -> bool {
    i > 0 && {
        let above = lines[i - 1].trim();
        !above.is_empty() && above.chars().all(|c| c == claude_screen::BORDER)
    }
}

/// The menu's highlighted option row: an *indented* `❯`-led row that
/// is not the boxed input line. Menu rows sit inside the dialog's
/// inset (` ❯ 1. Yes`); a column-0 `❯` row is the input box or a
/// transcript echo of the operator's own submitted text — never a
/// menu highlight.
fn sel_row(lines: &[&str], i: usize) -> bool {
    let l = lines[i];
    l.len() != l.trim_start().len() && l.trim_start().starts_with('❯') && !boxed(lines, i)
}

/// The live menu region is the rows BELOW the last boxed `❯` input
/// prompt. A real permission menu renders in place of the box's
/// interior — it can sit under a still-drawn prompt, but never above
/// one: a transcript that quotes a menu verbatim always has the live
/// input box below the quote. That frame position is the corroboration
/// screen-local text cannot fake.
fn menu_window<'a>(lines: &'a [&'a str]) -> &'a [&'a str] {
    let floor = (0..lines.len())
        .rev()
        .find(|i| boxed(lines, *i) && lines[*i].trim_start().starts_with(claude_screen::PROMPT));
    match floor {
        Some(f) => &lines[f + 1..],
        None => lines,
    }
}

/// The open option block as `(selected row, block start, block end,
/// corroborated)`. The block is the contiguous run of rows around the
/// highlighted `❯` row where an option row is `❯`-led at the
/// highlight's indent or carries its text at the highlight's column
/// (subject and context rows sit at other indents). A lone `❯` row is
/// not a one-option menu — a real menu always lists at least one
/// sibling option — so the block must reach at least two rows or it is
/// transcript text, not a menu.
///
/// Even so, an indented transcript `❯` plus a same-column sibling
/// satisfies the shape — the block is *corroborated* only by what a
/// transcript cannot fake at the same indent: a numbered option inside
/// the run, or the anchored menu legend directly below it (at most one
/// blank row between, as real menus render). An uncorroborated block
/// never sets `approval_menu` and is never answered.
fn option_block(lines: &[&str]) -> Option<(usize, usize, usize, bool)> {
    // Only rows below the live input box can be menu structure —
    // a quoted menu sitting above the box is transcript text.
    let lines = menu_window(lines);
    // The LAST non-boxed `❯` row: a transcript `❯` echo above the menu
    // would otherwise be mistaken for the highlight.
    let sel = (0..lines.len()).rev().find(|i| sel_row(lines, *i))?;
    // The column the highlighted option's text starts at — `❯` is
    // one glyph plus its trailing space.
    let sel_indent = lines[sel].find('❯').unwrap_or(0);
    let sel_col = sel_indent + 2;
    let opt = |i: usize| -> bool {
        let l = lines[i];
        let t = l.trim_start();
        // Blank, border and hint rows end the block. Anchor rows
        // are *not* excluded — `Yes, I trust this folder` is both
        // an anchor and a real option; subject rows sit at a
        // different column than options anyway.
        if t.is_empty()
            || t.chars().all(|c| c == claude_screen::BORDER)
            || claude_screen::HINT.iter().any(|h| hint_row(l, h))
        {
            return false;
        }
        (t.starts_with('❯') && l.find('❯') == Some(sel_indent))
            || (l.len() - t.len() == sel_col && !t.starts_with('$') && !t.starts_with("Tip:"))
    };
    let mut start = sel;
    while start > 0 && opt(start - 1) {
        start -= 1;
    }
    let mut end = sel;
    while end + 1 < lines.len() && opt(end + 1) {
        end += 1;
    }
    if end - start + 1 < 2 {
        return None;
    }
    let numbered = (start..=end).any(|i| option_line(lines[i]).is_some());
    let legend_below = lines[end + 1..]
        .iter()
        .take(2)
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| claude_screen::HINT.iter().any(|h| hint_row(l, h)));
    Some((sel, start, end, numbered || legend_below))
}

/// The line naming what the menu asks. A `Do you want to proceed?`
/// prompt carries its detail rows above the question — the command
/// first, then its description — so the furthest row inside the block
/// names the command. Trust dialogs name themselves by the anchor
/// row (`Quick safety check: …`).
fn menu_subject(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.trim_end().lines().collect();
    let q = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("Do you want to proceed?"));
    if let Some(q) = q {
        let mut subject = None;
        let mut blanks = 0;
        for l in lines[..q].iter().rev() {
            let t = l.trim();
            if t.is_empty() {
                blanks += 1;
                if blanks > 1 {
                    break;
                }
                continue;
            }
            if t.starts_with("Tip:") || t.chars().all(|c| c == claude_screen::BORDER) {
                break;
            }
            subject = Some(t.chars().take(100).collect());
            blanks = 0;
        }
        if subject.is_some() {
            return subject;
        }
    }
    lines
        .iter()
        .rev()
        .take(MENU_LINES)
        .find(|l| {
            let t = l.trim_start();
            t.starts_with("Quick safety check") || t.starts_with("Do you trust")
        })
        .map(|l| l.trim().chars().take(100).collect())
}

/// A running turn's status row: flush left, a [`claude_screen::SPINNER`]
/// glyph, a space, then a verb ending in `…`. A finished turn's row
/// keeps the glyph but has no ellipsis (`✻ Crunched for 18s · done`),
/// and transcript output quoting the row is indented, so neither
/// matches.
fn spinner_row(line: &str) -> bool {
    let mut chars = line.chars();
    let (Some(glyph), Some(' ')) = (chars.next(), chars.next()) else {
        return false;
    };
    let Some(stem) = claude_screen::SPINNER
        .contains(&glyph)
        .then(|| chars.as_str().split_whitespace().next())
        .flatten()
        .and_then(|verb| verb.strip_suffix('…'))
    else {
        return false;
    };
    stem.chars().next().is_some_and(char::is_alphabetic)
        && stem
            .chars()
            .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
}

/// Reduce a captured Claude screen to gate facts. The last boxed `❯`
/// line is the input box; text after it is a draft — except ghost
/// suggestion text, which a cursor parked at the box's start gives
/// away. Menus and busy markers win over prompt parsing — a `❯` leads
/// the first menu option too — and both are only read in the bottom
/// status region: the transcript above can legitimately print these
/// same strings (a completed turn's `✻ Cogitated …` line, source
/// text) without the pane being busy at all. The region is anchored
/// at the last NON-BLANK row — `capture-pane` pads to pane height, so
/// a young session on a tall pane has blank rows below the real
/// content. A running turn's spinner row is read by position instead:
/// flush left between the box and the last transcript item.
pub fn analyze_claude(screen: &str, cursor: Option<(u32, u32)>) -> Probe {
    analyze_frame(screen, None, cursor)
}

/// [`analyze_claude`] over a styled capture (`capture-pane -e`) — what
/// the adapter probes with. The attributes settle the input line: its
/// SGR-dim cells are a prompt suggestion or placeholder, never input,
/// and any undimmed text after the `❯` is a draft — typed text after a
/// suggestion, a partially dim line, or a draft whose cursor was moved
/// back to the start (where the plain-text cursor heuristic misreads
/// it as ghost). The cursor is not consulted.
pub fn analyze_claude_styled(styled: &str, cursor: Option<(u32, u32)>) -> Probe {
    let frame = super::sgr::parse(styled);
    analyze_frame(&frame.plain, Some(&frame.undimmed), cursor)
}

/// The shared reduction: `screen` is plain text; `undimmed` is the same
/// frame minus its dim cells when the capture carried attributes.
fn analyze_frame(screen: &str, undimmed: Option<&str>, cursor: Option<(u32, u32)>) -> Probe {
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
    // Menu evidence lives in the wider menu window — a permission
    // prompt plus a still-visible input box can push its rows above
    // the busy anchor. Anchors decide alone but only matched on the
    // trimmed row's leading text; hints (footer fragments, matched
    // row-anchored) need real menu structure beside them — numbered
    // option rows or the highlighted `❯` option row — so a transcript
    // quoting these strings stays text, never a menu.
    let menu_lines: Vec<&str> = content
        .lines()
        .rev()
        .take(MENU_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    // Only rows below the last boxed `❯` prompt can be menu structure:
    // a transcript quoting a real menu sits above the still-live input
    // box — the frame position is the corroboration its text cannot
    // fake. The slice applies to every menu scan uniformly.
    let menu_lines: &[&str] = menu_window(&menu_lines);
    // Natural-language anchors are ordinary sentence text — an
    // indented transcript row that starts with `Do you want to
    // proceed?` is identical in shape to the real prompt, so the
    // anchor only decides beside real menu structure: a numbered
    // option run, a highlighted `❯` option, or an anchored legend
    // row. Numbered rows must form a real option block — a `❯`-led
    // row inside the run or the menu legend right after it — a
    // markdown list in transcript text satisfies neither.
    let anchor = menu_lines.iter().any(|l| {
        let t = l.trim_start();
        claude_screen::ANCHOR.iter().any(|a| t.starts_with(a))
    });
    let numbered = numbered_block(menu_lines);
    // `highlighted` means a real option block *with corroboration* —
    // an indented `❯` plus a same-column sibling is transcript-fakeable,
    // so the block alone never suffices (numbered rows inside it or the
    // legend directly below are what count).
    let highlighted = option_block(menu_lines).is_some_and(|(.., c)| c);
    let hints = claude_screen::HINT
        .iter()
        .filter(|h| menu_lines.iter().any(|l| hint_row(l, h)))
        .count();
    // A lone legend row is quotable text too — it corroborates only as
    // a pair of distinct hints, or sitting within a row or two of the
    // anchor it belongs to (a `cat`ed doc puts them far apart).
    let hint_near_anchor = menu_lines.iter().enumerate().any(|(i, l)| {
        claude_screen::HINT.iter().any(|h| hint_row(l, h))
            && (i.saturating_sub(2)..=i + 2).any(|j| {
                menu_lines.get(j).is_some_and(|a| {
                    let t = a.trim_start();
                    claude_screen::ANCHOR.iter().any(|s| t.starts_with(s))
                })
            })
    });
    let legend_ok = hints >= 2 || hint_near_anchor;
    let approval_menu = (anchor && (numbered.len() >= 2 || highlighted || legend_ok))
        || (legend_ok && (numbered.len() >= 2 || highlighted));
    let interrupt_marker = claude_screen::BUSY.iter().any(|m| tail.contains(m));
    // The input box is a `❯`-leading line whose previous row is the
    // `─` border — menu option lists lead with `❯` too but are never
    // boxed, and transcript `❯` echoes have no border either.
    let lines: Vec<&str> = content.lines().collect();
    let mut prompt: Option<(usize, &str)> = None;
    for (i, line) in lines.iter().enumerate() {
        let boxed = i > 0 && {
            let above = lines[i - 1].trim();
            !above.is_empty() && above.chars().all(|c| c == claude_screen::BORDER)
        };
        if boxed && line.trim_start().starts_with(claude_screen::PROMPT) {
            prompt = Some((i, line));
        }
    }
    let prompt_visible = prompt.is_some();
    // The live status row sits between the box's top border and the
    // last transcript item (`●`) — the frame position a quoted row
    // cannot take.
    let spinner = prompt.is_some_and(|(row, _)| {
        lines[..row.saturating_sub(1)]
            .iter()
            .rev()
            .take(SPINNER_REACH)
            .take_while(|l| !l.starts_with('●'))
            .any(|l| spinner_row(l))
    });
    let busy_marker = interrupt_marker || spinner;
    let draft_of = |l: &str| {
        l.trim_start()
            .trim_start_matches(claude_screen::PROMPT)
            .trim()
            .to_string()
    };
    let input_nonempty = match (prompt, undimmed) {
        // Attributes known: only undimmed text on the input row is a
        // draft (row `i` of `undimmed` is row `i` of `screen`).
        (Some((row, _)), Some(u)) => u.lines().nth(row).is_some_and(|l| !draft_of(l).is_empty()),
        // Plain capture: dim ghost text in an empty box never moves the
        // cursor off the prompt start — a typed draft normally does.
        // Unknown cursor counts as a real draft (conservative).
        (Some((row, l)), None) => {
            let ghost = cursor.is_some_and(|(x, y)| y as usize == row && x <= 2);
            !draft_of(l).is_empty() && !ghost
        }
        (None, _) => false,
    };
    let (idle, reason) = if approval_menu {
        (
            false,
            menu_subject(content).unwrap_or_else(|| "approval menu is open".to_string()),
        )
    } else if interrupt_marker {
        (
            false,
            "tui is busy (interrupt marker on screen)".to_string(),
        )
    } else if spinner {
        (
            false,
            "tui is busy (turn spinner above the input box)".to_string(),
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
        trust_prompt: false,
        steerable: false,
        queue_pending: false,
    }
}

/// One live entry in Claude's session registry.
struct SessionEntry {
    pid: u32,
    session_id: String,
    /// Process start ticks — a stale entry whose pid was recycled by
    /// an unrelated process fails this check instead of passing for
    /// live ownership.
    proc_start: Option<String>,
}

impl SessionEntry {
    /// Live iff `/proc/<pid>` exists and the recorded process start
    /// still matches — a pid-reused process is not this Claude.
    fn alive(&self) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)) else {
            return false;
        };
        match &self.proc_start {
            Some(want) => proc_start_ticks(&stat).as_deref() == Some(want.as_str()),
            None => true,
        }
    }
}

/// Field 22 (`starttime`, jiffies) of a `/proc/<pid>/stat` line —
/// matched against the registry's `procStart`. The `comm` field is
/// skipped by splitting after its closing paren (it may contain
/// spaces).
fn proc_start_ticks(stat: &str) -> Option<String> {
    let rest = stat.rsplit_once(')')?.1;
    // Fields after comm start at 3 (state): starttime is field 22.
    rest.split_whitespace().nth(19).map(str::to_string)
}

/// The pane-side environment scrub for a real `claude` launch — the
/// same rule as the managed adapter's (`EnvScrub::prefixes`), expressed
/// as `env -u` because the pane launches through a shell command, not
/// a Command env table. `CADENCE_ALIAS`/`CADENCE_STATE_DIR` stay: the
/// agent and the Stop hook need them for `cadence self`. So does the
/// daemon's tracker and profile ([`crate::adapter::DAEMON_CONTEXT_ENV`],
/// set on the pane with `-e`): a sandbox worker's `cadence issue …`
/// must stay in the sandbox.
fn scrubbed_env_names() -> Vec<String> {
    scrub_names(std::env::vars().map(|(k, _)| k))
}

/// [`scrubbed_env_names`] over an explicit list of inherited names.
fn scrub_names(inherited: impl Iterator<Item = String>) -> Vec<String> {
    const KEEP: &[&str] = &[
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CADENCE_ALIAS",
        "CADENCE_STATE_DIR",
    ];
    let mut names: Vec<String> = inherited
        .filter(|k| {
            (k.starts_with("CLAUDE_")
                || k.starts_with("CLAUDECODE")
                || k.starts_with("CODEX_")
                || k.starts_with("CADENCE_"))
                && !KEEP.contains(&k.as_str())
                && !crate::adapter::DAEMON_CONTEXT_ENV.contains(&k.as_str())
        })
        .collect();
    for name in crate::adapter::CLOUD_SECRET_ENV {
        if !names.iter().any(|have| have == name) {
            names.push((*name).to_string());
        }
    }
    names
}

/// `--settings` JSON wiring a Stop hook: when the TUI finishes a turn
/// it acks any running cadence message (`cadence self` names the id
/// and token; pane env supplies `CADENCE_ALIAS`/`CADENCE_STATE_DIR`).
/// Provider receipt is still not completion — the agent's own
/// `message result` remains authoritative.
fn stop_hook_settings() -> String {
    serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "o=$(cadence self 2>/dev/null) || exit 0; \
                        printf '%s' \"$o\" | jq -r '.running[]? | .id + \" \" + .turn_id' | \
                        while read -r id tok; do \
                        [ -n \"$id\" ] && [ -n \"$tok\" ] && \
                        cadence message ack \"$id\" --token \"$tok\" >/dev/null 2>&1; \
                        done; exit 0"
                }]
            }]
        }
    })
    .to_string()
}

/// The Claude profile: `claude` CLI argv (`--session-id` fresh,
/// `--resume` on reopen), session ownership via
/// `~/.claude/sessions/<pid>.json`, and the Claude screen analyzer.
pub struct ClaudeProfile {
    /// `~/.claude/sessions` — overridable in tests.
    sessions_dir: PathBuf,
    /// Launch command prefix: the `CADENCE_CLAUDE_TUI_COMMAND`
    /// override verbatim, else a PATH-resolved `claude`.
    command: String,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    /// `--allowedTools` replayed verbatim; `Bash(cadence *)` always
    /// heads the list so the agent can call the CLI unimpeded.
    allowed_tools: Vec<String>,
    /// A real `claude` binary (not the env override): wrap the pane
    /// command in the env scrub and wire the Stop hook.
    real: bool,
}

impl ClaudeProfile {
    /// Resolve the profile from the agent's stored params and the
    /// environment: the session registry dir (`CADENCE_CLAUDE_SESSIONS`
    /// for tests), the launch command (`CADENCE_CLAUDE_TUI_COMMAND`
    /// used verbatim — tests pass `python3 mock.py <sessions>` — else a
    /// `claude` found on PATH), and the permission/model/tool params
    /// replayed on every launch exactly as the managed adapter does.
    pub fn new(agent: &Agent, env: &ProviderEnv) -> Result<Self> {
        let sessions_dir = env
            .var("CADENCE_CLAUDE_SESSIONS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/sessions")
            });
        let (command, real) = match env.var("CADENCE_CLAUDE_TUI_COMMAND") {
            Some(cmd) if !cmd.is_empty() => (cmd, false),
            _ => (resolve_on_path("claude").map(|p| shlex_quote(&p))?, true),
        };
        let params = agent.params.clone().unwrap_or(Value::Null);
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.model.clone());
        let effort = params
            .get("effort")
            .and_then(Value::as_str)
            .map(str::to_string);
        let permission_mode = params
            .get("permission_mode")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut allowed_tools = vec!["Bash(cadence *)".to_string()];
        if let Some(list) = params.get("allowed_tools").and_then(Value::as_array) {
            allowed_tools.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        Ok(Self {
            sessions_dir,
            command,
            model,
            effort,
            permission_mode,
            allowed_tools,
            real,
        })
    }

    /// Every live session-registry entry — parsed `<pid>.json` files
    /// whose process is still running under the recorded start tick.
    /// Single-shot: callers that need resilience retry at their own
    /// decision point.
    fn live_entries(&self) -> Vec<SessionEntry> {
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| self.read_entry(&e.path()))
            .filter(SessionEntry::alive)
            .collect()
    }

    /// One `<pid>.json` entry: the filename pid is authoritative, and
    /// must match the file's own `pid` field — a corrupt or renamed
    /// file claims nothing.
    fn read_entry(&self, path: &Path) -> Option<SessionEntry> {
        let pid: u32 = path.file_stem()?.to_str()?.parse().ok()?;
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        if v.get("pid").and_then(Value::as_u64) != Some(u64::from(pid)) {
            return None;
        }
        let session_id = v.get("sessionId")?.as_str()?.to_string();
        let proc_start = v.get("procStart").and_then(|x| {
            x.as_str()
                .map(str::to_string)
                .or_else(|| x.as_u64().map(|n| n.to_string()))
        });
        Some(SessionEntry {
            pid,
            session_id,
            proc_start,
        })
    }
}

impl TuiProfile for ClaudeProfile {
    fn name(&self) -> &'static str {
        "Claude"
    }

    fn launch_command(&self, resume: Option<&str>) -> Result<String> {
        let mut argv = self.command.clone();
        match resume {
            Some(want) => argv.push_str(&format!(" --resume {}", shlex_quote(want))),
            None => argv.push_str(&format!(" --session-id {}", Uuid::new_v4())),
        }
        if let Some(model) = &self.model {
            argv.push_str(&format!(" --model {}", shlex_quote(model)));
        }
        if let Some(effort) = &self.effort {
            argv.push_str(&format!(" --effort {}", shlex_quote(effort)));
        }
        // `bypassPermissions` rides in the stored permission_mode param
        // (the CLI folds --bypass into it). It maps to the mode flag,
        // not --dangerously-skip-permissions: the legacy flag can stop
        // on an interactive consent warning before the session
        // registers, while the mode flag reaches the same state with
        // no dialog (observed 2.1.276).
        if let Some(mode) = &self.permission_mode {
            argv.push_str(&format!(" --permission-mode {}", shlex_quote(mode)));
        }
        for tool in &self.allowed_tools {
            argv.push_str(&format!(" --allowedTools {}", shlex_quote(tool)));
        }
        if self.real {
            argv.push_str(&format!(
                " --settings {}",
                shlex_quote(&stop_hook_settings())
            ));
            // The pane inherits the daemon's env via the tmux server —
            // scrub nested-session and other-provider variables out of
            // the claude child's environment (`env -u` per name).
            let mut wrapped = String::from("env");
            for name in scrubbed_env_names() {
                wrapped.push_str(&format!(" -u {name}"));
            }
            argv = format!("{wrapped} {argv}");
        }
        Ok(argv)
    }

    fn exit_banner(&self) -> &'static str {
        "Claude exited. This pane will close."
    }

    /// The native session whose live registry pid descends from
    /// `pane_pid`, or `None` when the pane owns no session.
    fn owned_session(&self, pane_pid: u32) -> Option<String> {
        self.live_entries()
            .into_iter()
            .find(|e| descends_from(e.pid, pane_pid))
            .map(|e| e.session_id)
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
                "pane has no live Claude session entry for '{want}'"
            ))),
            (None, Some(found)) => Ok(found),
            (None, None) => Err(Error::provider("pane exists but owns no Claude session")),
        }
    }

    /// Refuse takeover: a live registry entry for `session` outside our
    /// (future) pane means another TUI already owns it.
    fn refuse_takeover(&self, session: &str) -> Result<()> {
        if let Some(e) = self
            .live_entries()
            .into_iter()
            .find(|e| e.session_id == session)
        {
            return Err(Error::rejected(format!(
                "Claude session '{session}' is owned by another terminal \
                 (pid {}); close it first — no takeover",
                e.pid
            )));
        }
        Ok(())
    }

    /// The pane owns `native` while the registry entry's live pid
    /// descends from its pid — rechecked before every send.
    fn verify_ownership(&self, native: &str, pane_pid: u32) -> Result<()> {
        match self
            .live_entries()
            .into_iter()
            .find(|e| e.session_id == native)
        {
            Some(e) if descends_from(e.pid, pane_pid) => Ok(()),
            Some(e) => Err(Error::provider(format!(
                "Claude session '{native}' is owned by pid {} outside our pane",
                e.pid
            ))),
            None => Err(Error::provider(format!(
                "pane owns no live Claude session entry for '{native}'"
            ))),
        }
    }

    fn open_deadline(&self) -> Duration {
        OPEN_DEADLINE
    }

    fn analyze(&self, screen: &str, cursor: Option<(u32, u32)>) -> Probe {
        analyze_claude(screen, cursor)
    }

    fn analyze_styled(&self, styled: &str, cursor: Option<(u32, u32)>) -> Probe {
        analyze_claude_styled(styled, cursor)
    }

    /// The draft is the input box's interior: the last boxed `❯` row
    /// and any wrapped rows under it, down to the box's bottom `─`
    /// border. Dim cells anywhere in it (a prompt suggestion, a
    /// placeholder) refuse — ghost text must never pass for the draft,
    /// and no bottom border means the box is cut off.
    fn draft_rows(&self, styled: &str) -> std::result::Result<DraftView, String> {
        let frame = super::sgr::parse(styled);
        let plain: Vec<&str> = frame.plain.lines().collect();
        let undimmed: Vec<&str> = frame.undimmed.lines().collect();
        let border = |l: &str| {
            let t = l.trim();
            !t.is_empty() && t.chars().all(|c| c == claude_screen::BORDER)
        };
        let start = (1..plain.len())
            .rev()
            .find(|&i| {
                border(plain[i - 1]) && plain[i].trim_start().starts_with(claude_screen::PROMPT)
            })
            .ok_or("no Claude input box on screen")?;
        let end = (start + 1..plain.len()).find(|&i| border(plain[i])).ok_or(
            "the Claude input box has no bottom border on screen — the draft cannot be delimited",
        )?;
        if (start..end).any(|i| undimmed.get(i).map(|u| u.trim_end()) != Some(plain[i].trim_end()))
        {
            return Err(
                "dim text in the Claude input box — a suggestion or placeholder cannot be \
                 told apart from the draft"
                    .to_string(),
            );
        }
        let mut rows = vec![plain[start]
            .trim_start()
            .trim_start_matches(claude_screen::PROMPT)
            .trim()
            .to_string()];
        rows.extend(plain[start + 1..end].iter().map(|l| l.trim().to_string()));
        // The border spans the box; a row's text is at most that less
        // the `❯ ` prompt (continuation rows are indented as far).
        let width = plain[end].trim().chars().count().saturating_sub(2);
        Ok(DraftView {
            rows,
            width: Some(width),
        })
    }

    fn respond_rejection(&self) -> &'static str {
        "pty endpoints have no approval channel — answer Claude \
         permission prompts in the terminal itself"
    }

    /// Numbered permission menus take the option's digit key; an
    /// unnumbered select (the trust dialog) falls back to arrows +
    /// Enter, bounded by the option rows on screen.
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
        // Only a qualified option run counts — a markdown numbered
        // list in the transcript would otherwise take the numbered
        // branch and answer a digit an unnumbered dialog ignores.
        let numbered: Vec<u32> = numbered_block(&region);
        let n: u32 = choice.parse().map_err(|_| {
            Error::rejected(format!(
                "'{choice}' is not a menu index — Claude menus take the \
                 option's printed number"
            ))
        })?;
        // A one-row "numbered run" is a stray `N.` line, not a menu —
        // real menus list at least two options.
        if numbered.len() >= 2 {
            if !numbered.contains(&n) {
                return Err(Error::rejected(format!(
                    "no option {n} on this menu — it lists {}",
                    numbered
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            // A single digit is one keypress. A multi-digit index
            // must never reach tmux as one literal — `send-keys
            // "10"` presses `1` then `0`, and a menu that selects
            // on the first press would pick option 1 while `0`
            // leaks as input. Navigate from the highlight instead.
            if n < 10 {
                return Ok(vec![n.to_string()]);
            }
            let sel = (0..region.len()).rev().find(|i| sel_row(&region, *i));
            let cur = sel
                .and_then(|i| option_line(region[i]))
                .filter(|c| numbered.contains(c))
                .ok_or_else(|| {
                    Error::rejected(
                        "cannot locate the highlighted option on this \
                         menu — answer it in the pane",
                    )
                })?;
            let want = numbered.iter().position(|o| *o == n).unwrap();
            let at = numbered.iter().position(|o| *o == cur).unwrap();
            let (dir, steps) = if want >= at {
                ("Down", want - at)
            } else {
                ("Up", at - want)
            };
            let mut keys = vec![dir.to_string(); steps];
            keys.push("Enter".to_string());
            return Ok(keys);
        }
        // Unnumbered select: navigate the whole printed option block
        // up and down from the highlighted `❯` row — never just the
        // suffix below it. `option_block` requires a real sibling set
        // around an indented `❯` row, so a transcript `❯` echo beside
        // anchor text cannot be answered as a menu.
        let (sel, start, end, corroborated) = option_block(&region).ok_or_else(|| {
            Error::rejected(
                "cannot locate the option rows on this menu — answer \
                 it in the pane",
            )
        })?;
        // An indentation-inferred block is transcript-fakeable — only
        // a numbered option or the anchored legend directly below makes
        // it a menu worth keying.
        if !corroborated {
            return Err(Error::rejected(
                "the option block is inferred from indentation only — no \
                 numbered option or legend corroborates it; answer it \
                 in the pane",
            ));
        }
        let count = (end - start + 1) as u32;
        if n == 0 || n > count {
            return Err(Error::rejected(format!(
                "no option {n} on this menu — it lists {count}"
            )));
        }
        // Navigate from the highlighted row — it is not necessarily
        // the first option.
        let want = (n - 1) as usize;
        let cur = sel - start;
        let (dir, steps) = if want >= cur {
            ("Down", want - cur)
        } else {
            ("Up", cur - want)
        };
        let mut keys = vec![dir.to_string(); steps];
        keys.push("Enter".to_string());
        Ok(keys)
    }

    /// Claude Code interrupts a running turn on Esc (`esc to interrupt`);
    /// `C-c` on an idle prompt arms the exit instead.
    fn interrupt_keys(&self) -> &'static [&'static str] {
        &["Escape"]
    }

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_claude, analyze_claude_styled, proc_start_ticks, scrub_names, ClaudeProfile,
        STATUS_LINES,
    };
    use crate::adapter::pty::profile::TuiProfile;
    use std::path::PathBuf;

    /// Real captures from a live claude 2.1.275 pane (CAD-17),
    /// committed under tests/fixtures/claude-tui/.
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/claude-tui/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// The 0-based row of the boxed `❯` line — where the real pane
    /// cursor sits — for fixtures whose exact row varies.
    fn boxed_prompt_row(screen: &str) -> usize {
        let lines: Vec<&str> = screen.trim_end().lines().collect();
        let mut row = None;
        for (i, l) in lines.iter().enumerate() {
            let boxed = i > 0 && {
                let above = lines[i - 1].trim();
                !above.is_empty() && above.chars().all(|c| c == '─')
            };
            if boxed && l.trim_start().starts_with('❯') {
                row = Some(i);
            }
        }
        row.expect("fixture has no boxed ❯ line")
    }

    #[test]
    fn idle_prompt_is_pasteable() {
        let screen = fixture("idle.txt");
        let p = analyze_claude(&screen, Some((2, boxed_prompt_row(&screen) as u32)));
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(p.prompt_visible && !p.input_nonempty);
        assert!(!p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn typed_draft_is_not_idle() {
        let screen = fixture("draft.txt");
        let row = boxed_prompt_row(&screen) as u32;
        // Cursor after the draft text — a real staged input.
        let p = analyze_claude(&screen, Some((35, row)));
        assert!(!p.idle && p.input_nonempty);
        assert_eq!(p.reason, "unsubmitted text in the input line");
    }

    #[test]
    fn ghost_suggestion_reads_as_empty_input() {
        // Observed live: an idle box shows a dim suggestion
        // (`❯  ls -l …`, SGR-dim — indistinguishable from a draft in
        // plain capture) while the cursor stays at the prompt start.
        // The cursor is what proves the input is really empty.
        let screen = fixture("ghost.txt");
        let row = boxed_prompt_row(&screen) as u32;
        let p = analyze_claude(&screen, Some((2, row)));
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(!p.input_nonempty, "ghost text must not read as a draft");
        // No cursor information → the visible text counts as a real
        // draft (conservative: a real staged draft is never claimed over).
        let blind = analyze_claude(&screen, None);
        assert!(!blind.idle && blind.input_nonempty);
        // A cursor sitting past the prompt start means real input.
        let typed = analyze_claude(&screen, Some((9, row)));
        assert!(!typed.idle && typed.input_nonempty);
    }

    /// Live `capture-pane -e` frames (Claude Code 2.1.280, CAD-294):
    /// `suggestion.ansi` is a cadence lane's idle box showing a dim
    /// prompt suggestion, `placeholder.ansi` a fresh session's dim
    /// `Try "…"` placeholder (dim words, undimmed spaces between),
    /// `typed.ansi` a typed draft, and `typed-home.ansi` the same draft
    /// after Home — its cursor back at the prompt start.
    #[test]
    fn styled_dim_suggestion_probes_idle() {
        for (name, row) in [("suggestion.ansi", 32), ("placeholder.ansi", 10)] {
            let styled = fixture(name);
            // The attributes decide — the verdict holds with the cursor
            // at the prompt start, unknown, or anywhere else.
            for cursor in [Some((2, row)), None, Some((40, row + 3))] {
                let p = analyze_claude_styled(&styled, cursor);
                assert!(p.idle, "{name} {cursor:?}: {p:?}");
                assert!(
                    p.prompt_visible && !p.input_nonempty,
                    "{name} {cursor:?}: {p:?}"
                );
            }
        }
    }

    #[test]
    fn styled_typed_draft_is_not_idle() {
        for (name, cursor) in [("typed.ansi", (26, 10)), ("typed-home.ansi", (2, 10))] {
            let p = analyze_claude_styled(&fixture(name), Some(cursor));
            assert!(!p.idle && p.input_nonempty, "{name}: {p:?}");
            assert_eq!(p.reason, "unsubmitted text in the input line", "{name}");
        }
    }

    /// The live suggestion frame with its input row replaced by `line`.
    fn suggestion_with_input(line: &str) -> String {
        let frame = fixture("suggestion.ansi");
        let mut rows: Vec<&str> = frame.lines().collect();
        let row = rows.iter().rposition(|l| l.contains('❯')).unwrap();
        rows[row] = line;
        rows.join("\n")
    }

    #[test]
    fn styled_input_line_edge_cases() {
        let draft = [
            // Typed text after a dim suggestion is still a draft.
            "\x1b[39m❯\u{a0}\x1b[2mghost\x1b[0m typed",
            // A partially dim line: typed text with a dim completion.
            "\x1b[39m❯\u{a0}fix the \x1b[2mbug in foo\x1b[0m",
            // Normal intensity (22) ends the dim run.
            "\x1b[39m❯\u{a0}\x1b[2mghost\x1b[22mtyped",
            // 256-colour / RGB arguments of 2 are colours, not dim.
            "\x1b[39m❯\u{a0}\x1b[38;5;2mtyped\x1b[39m",
            "\x1b[39m❯\u{a0}\x1b[38;2;2;2;2mtyped\x1b[39m",
        ];
        for line in draft {
            let p = analyze_claude_styled(&suggestion_with_input(line), Some((2, 32)));
            assert!(!p.idle && p.input_nonempty, "{line:?}: {p:?}");
        }
        let ghost = [
            // Dim combined with a colour, in either order.
            "\x1b[39m❯\u{a0}\x1b[2;38;5;246mghost\x1b[0m",
            "\x1b[39m❯\u{a0}\x1b[38;5;246;2mghost\x1b[0m",
            // Bold on top of dim is still dim.
            "\x1b[39m❯\u{a0}\x1b[2m\x1b[1mghost\x1b[0m",
        ];
        for line in ghost {
            let p = analyze_claude_styled(&suggestion_with_input(line), None);
            assert!(p.idle && !p.input_nonempty, "{line:?}: {p:?}");
        }
    }

    #[test]
    fn approval_menu_wins_over_prompt_shape() {
        // The permission select's first option leads with `❯` too —
        // menu detection must outrank prompt parsing or it reads as a
        // draft. The menu replaces the box entirely (no boxed ❯ here).
        let p = analyze_claude(&fixture("approval.txt"), None);
        assert!(!p.idle);
        assert!(p.approval_menu);
        // The reason names the command being approved — the furthest
        // detail row above the proceed question.
        assert_eq!(p.reason, "touch /tmp/claude-obs-marker-7");
    }

    #[test]
    fn trust_prompt_is_a_menu() {
        // The directory-trust dialog — this build never surfaces it,
        // but an older/newer one still reads as a menu, never idle.
        let p = analyze_claude(&fixture("trust.txt"), None);
        assert!(!p.idle && p.approval_menu, "{:?}", p);
    }

    #[test]
    fn busy_markers_block_even_with_prompt() {
        // busy.txt is a real capture (the smoke's sleep turn): the
        // live frame landed just after the spinner cleared, so the
        // observed spinner line — `✻ Churning… (esc to interrupt ·
        // Ns)` — is spliced back at its real position above the box.
        let p = analyze_claude(&fixture("busy.txt"), None);
        assert!(!p.idle && p.busy_marker, "{}", p.reason);
        assert_eq!(p.reason, "tui is busy (interrupt marker on screen)");
        // Marker strings inside the status region block regardless of
        // which transcript precedes them.
        let idle = fixture("idle.txt");
        for marker in ["(esc to interrupt · 4s", "⎿  Waiting…"] {
            let screen = format!("{idle}\nworking {marker}");
            let p = analyze_claude(&screen, None);
            assert!(!p.idle && p.busy_marker, "{marker}: {}", p.reason);
        }
    }

    /// CAD-285: Claude Code 2.1.280 dropped `esc to interrupt` from the
    /// running turn's status row. spinner.txt is a live capture of a
    /// working pane (cad-284, 2026-09-23) whose only busy evidence is
    /// that row above an empty box; it probed idle.
    #[test]
    fn running_turn_spinner_above_the_box_is_busy() {
        let busy = fixture("spinner.txt");
        let p = analyze_claude(&busy, Some((2, 14)));
        assert!(!p.idle && p.busy_marker, "{p:?}");
        assert_eq!(p.reason, "tui is busy (turn spinner above the input box)");
        // Every spinner frame and status shape observed on 2.1.280.
        let observed = "✻ Warping… (1m 35s · ↓ 6.5k tokens · thinking with high effort)";
        for row in [
            "✢ Thundering… (4m 5s · ↓ 22.6k tokens · thinking with high effort)",
            "· Warping… (11s · ↓ 595 tokens)",
            "✶ Recombobulating… (4s · ↓ 217 tokens · thought for 1s)",
            "✳ Tomfoolering… (8s · ↓ 400 tokens · thinking with high effort)",
            "✽ Schlepping…",
        ] {
            let p = analyze_claude(&busy.replace(observed, row), None);
            assert!(!p.idle && p.busy_marker, "{row}: {p:?}");
        }
        // A todo list rendered under the spinner keeps it busy.
        let todos = busy.replace(
            observed,
            &format!(
                "{observed}\n  ⎿  ☒ Read the store\n     ☐ Write the test\n     ☐ Run the suite"
            ),
        );
        assert!(analyze_claude(&todos, None).busy_marker);
    }

    /// A finished turn keeps the glyph but loses the ellipsis
    /// (`✻ Crunched for 18s · done 11:57 AM`, live capture from an idle
    /// cad-144 pane), and a spinner-shaped row is only the live status
    /// row when it is flush left below the last transcript item.
    #[test]
    fn finished_turn_row_and_quoted_spinners_are_not_busy() {
        let done = fixture("done.txt");
        let p = analyze_claude(&done, Some((2, 9)));
        assert!(p.idle && !p.busy_marker, "{p:?}");
        for row in [
            "✻ Cooked for 50m 41s · done 10:02 AM",
            "✻ Baked for 2s · done 12:30 PM",
        ] {
            let p = analyze_claude(
                &done.replace("✻ Crunched for 18s · done 11:57 AM", row),
                None,
            );
            assert!(!p.busy_marker, "{row}: {p:?}");
        }
        // Quoted in tool output (indented) or above the last transcript
        // item, the same text is transcript, not the live status row.
        let spinner = "✻ Warping… (1m 35s · ↓ 6.5k tokens)";
        let indented = done.replace(
            "✻ Crunched for 18s · done 11:57 AM",
            &format!("  ⎿  {spinner}\n\n✻ Crunched for 18s · done 11:57 AM"),
        );
        assert!(!analyze_claude(&indented, None).busy_marker);
        let above_item = format!("{spinner}\n\n{done}");
        assert!(!analyze_claude(&above_item, None).busy_marker);
    }

    #[test]
    fn menu_option_is_not_the_input_box() {
        // `❯ 1. Yes` inside an approval menu is an option, not a
        // boxed input line — only the bordered box counts.
        let p = analyze_claude(&fixture("approval.txt"), Some((2, 30)));
        assert!(!p.prompt_visible, "{:?}", p);
        assert!(p.approval_menu);
    }

    #[test]
    fn markers_in_transcript_do_not_count() {
        // The transcript may legitimately print the marker strings
        // (this repo's own source does). Only the bottom status region
        // is authoritative — a marker scrolled above it must not stall
        // an idle pane.
        let mut screen = String::new();
        for marker in ["esc to interrupt", "Do you want to proceed?", "Waiting…"] {
            screen.push_str(&format!("transcript line: {marker}\n"));
        }
        for i in 0..STATUS_LINES {
            screen.push_str(&format!("ordinary output row {i}\n"));
        }
        screen.push_str(&fixture("idle.txt"));
        for blanks in ["", "\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n\n"] {
            let padded = format!("{screen}{blanks}");
            let p = analyze_claude(&padded, Some((2, 0)));
            assert!(p.idle, "{} / {}", p.idle, p.reason);
            assert!(!p.busy_marker && !p.approval_menu);
        }
    }

    #[test]
    fn trailing_blank_rows_do_not_hide_approval() {
        // capture-pane pads to pane height: a young session on a tall
        // pane leaves blank rows below the real content. The status
        // region must anchor at the last non-blank row.
        let screen = format!("{}{}", fixture("approval.txt"), "\n".repeat(30));
        let p = analyze_claude(&screen, None);
        assert!(!p.idle && p.approval_menu, "{:?}", p);
    }

    #[test]
    fn proc_start_field_22() {
        // (comm may contain spaces) — starttime is field 22, i.e. the
        // 20th whitespace field after the closing paren.
        let stat =
            "1234 (node) S 1 1234 1234 0 -1 4194304 500 0 0 0 10 5 0 0 20 0 1 0 130881239 100 200";
        assert_eq!(proc_start_ticks(stat).as_deref(), Some("130881239"));
    }

    /// A profile for `approval_answer` tests — the method only reads
    /// the screen, so the launch fields are dummies.
    fn profile() -> ClaudeProfile {
        ClaudeProfile {
            sessions_dir: PathBuf::from("/tmp"),
            command: "claude".to_string(),
            model: None,
            effort: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            real: false,
        }
    }

    /// CAD-152: the Claude draft is the boxed `❯` row and its wrapped
    /// rows down to the bottom border — read from a live capture; dim
    /// text in the box (a suggestion) and a box cut off below refuse.
    #[test]
    fn draft_rows_read_the_boxed_input() {
        let prof = profile();
        assert_eq!(
            prof.draft_rows(&fixture("draft.txt")).unwrap().rows,
            vec!["Review the deploy plan and reply"]
        );
        let rule = "─".repeat(40);
        let wrapped = format!("● done\n\n{rule}\n❯\u{a0}Kickoff AOS-11: read the\n  brief and report\n{rule}\n  [Opus]");
        let draft = prof.draft_rows(&wrapped).unwrap();
        assert_eq!(
            draft.rows,
            vec!["Kickoff AOS-11: read the", "brief and report"]
        );
        // The border's width less the `❯ ` prompt.
        assert_eq!(draft.width, Some(38));
        let err = prof.draft_rows(&fixture("suggestion.ansi")).unwrap_err();
        assert!(err.contains("dim text"), "{err}");
        let cut = format!("{rule}\n❯ Kickoff AOS-11: read the brief");
        let err = prof.draft_rows(&cut).unwrap_err();
        assert!(err.contains("no bottom border"), "{err}");
    }

    /// The trust dialog's option block — ` Security guide` sits right
    /// above the highlight at a different indent and must never count
    /// as an option.
    fn trust_menu() -> &'static str {
        " Quick safety check: Is this a project you trust?\n \n Security guide\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel\n"
    }

    #[test]
    fn unnumbered_answer_navigates_the_block() {
        let prof = profile();
        assert_eq!(
            prof.approval_answer(trust_menu(), "2").unwrap(),
            vec!["Down", "Enter"]
        );
        assert_eq!(
            prof.approval_answer(trust_menu(), "1").unwrap(),
            vec!["Enter"]
        );
    }

    #[test]
    fn answer_counts_options_above_the_highlight() {
        // The highlight is on the SECOND printed option — `answer 1`
        // must move Up to the first row, never confirm `No, exit`.
        let screen = " Security guide\n   Yes, I trust this folder\n ❯ No, exit\n Enter to confirm · Esc to cancel\n";
        let prof = profile();
        assert_eq!(
            prof.approval_answer(screen, "1").unwrap(),
            vec!["Up", "Enter"]
        );
        assert_eq!(prof.approval_answer(screen, "2").unwrap(), vec!["Enter"]);
    }

    #[test]
    fn answer_index_is_bounded_by_the_visible_block() {
        let prof = profile();
        for choice in ["3", "4000000000", "0"] {
            assert!(
                prof.approval_answer(trust_menu(), choice).is_err(),
                "{choice} must refuse"
            );
        }
        // Footer chrome with no option block refuses outright — never
        // a blind arrow walk.
        let footer_only = "transcript\n Enter to confirm · Esc to cancel\n";
        assert!(prof.approval_answer(footer_only, "1").is_err());
        assert!(prof.approval_answer(footer_only, "4000000000").is_err());
    }

    #[test]
    fn multi_digit_answer_navigates_instead_of_typing() {
        // `send-keys "10"` would press `1` (selecting option 1 on a
        // digit-key menu) and leak `0` — a multi-digit index arrows
        // from the highlighted row instead.
        let menu = " Do you want to proceed?\n ❯ 1. Yes\n   2. A\n   3. B\n   4. C\n   5. D\n   6. E\n   7. F\n   8. G\n   9. H\n   10. No\n Enter to confirm · Esc to cancel\n";
        let prof = profile();
        assert_eq!(
            prof.approval_answer(menu, "10").unwrap(),
            vec!["Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Down", "Enter"]
        );
        assert_eq!(prof.approval_answer(menu, "3").unwrap(), vec!["3"]);
        assert!(prof.approval_answer(menu, "11").is_err());
        // A numbered menu with no `❯` highlight refuses rather than
        // guessing where the arrows start from.
        let no_highlight = " Do you want to proceed?\n   1. Yes\n   2. A\n   3. B\n   4. C\n   5. D\n   6. E\n   7. F\n   8. G\n   9. H\n   10. No\n Enter to confirm · Esc to cancel\n";
        assert!(prof.approval_answer(no_highlight, "10").is_err());
    }

    #[test]
    fn quoted_anchor_text_is_not_a_menu() {
        // The anchor strings quoted mid-line in a transcript stay
        // text — detection matches on the trimmed row's leading text.
        let idle = fixture("idle.txt");
        for quoted in [
            "earlier it asked \"Do you want to proceed?\" — transcript",
            "the log shows \"Quick safety check\" in passing",
            "docs mention \"Yes, I trust this folder\" as an option",
            "it printed \"Do you trust the files in this folder?\" once",
        ] {
            let p = analyze_claude(&format!("{idle}\n{quoted}"), Some((2, 0)));
            assert!(!p.approval_menu, "{quoted}: {:?}", p);
        }
    }

    #[test]
    fn quoted_footer_text_is_not_a_menu() {
        // Footer fragments mid-line are not menu rows — a menu needs
        // the anchored legend or the `❯` highlight beside them.
        let idle = fixture("idle.txt");
        for quoted in [
            "press \"Esc to cancel\" to dismiss it",
            "or use \"Enter to confirm\" per the docs",
        ] {
            let p = analyze_claude(&format!("{idle}\n{quoted}"), Some((2, 0)));
            assert!(!p.approval_menu, "{quoted}: {:?}", p);
        }
    }

    #[test]
    fn indented_anchor_without_structure_is_not_a_menu() {
        // The round-3 livelock: a transcript row that LEADS with the
        // anchor once indented satisfies the trimmed `starts_with`,
        // and the bare anchor used to flip the pane to
        // `approval_menu` — gating every send forever. With no
        // option block and no legend beside it the row is inert.
        let idle = fixture("idle.txt");
        for line in [
            "    Do you want to proceed?",
            "      Quick safety check — mentioned above",
            "   Do you trust the files in this folder?",
        ] {
            let p = analyze_claude(&format!("{idle}\n{line}"), Some((2, 0)));
            assert!(!p.approval_menu, "{line}: {:?}", p);
        }
    }

    #[test]
    fn markdown_numbered_list_is_not_a_menu() {
        // `option_line` matches any `N.` row — a markdown list in the
        // transcript must not parse as menu options: no `❯`-led row
        // inside the run and no legend after it.
        let idle = fixture("idle.txt");
        let list = "the plan:\n    1. gather context\n    2. draft the patch\n    3. run tests\n";
        let p = analyze_claude(&format!("{idle}\n{list}"), Some((2, 0)));
        assert!(!p.approval_menu, "{p:?}");
        // And it cannot force the numbered answer path: a list above
        // a real unnumbered dialog leaves the ❯ block in charge.
        let screen = " 1. gather\n 2. draft\n 3. test\n Quick safety check\n ❯ No, exit\n   Yes, I trust this folder\n Enter to confirm · Esc to cancel\n";
        let prof = profile();
        assert_eq!(
            prof.approval_answer(screen, "2").unwrap(),
            vec!["Down", "Enter"]
        );
    }

    #[test]
    fn transcript_prompt_echoes_are_not_a_menu() {
        // Captured verbatim from a live pane: a submitted prompt's
        // `❯`-echo sits at column 0 and a wrapped continuation line
        // can start with the anchor — together they once parsed as a
        // two-row menu and livelocked sends. Column-0 `❯` rows are
        // echoes or the input box, never a menu highlight.
        let screen = "❯ xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n  Do you want to proceed?\n● API Error: safeguards flagged this message\n\n❯ \n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        let prof = profile();
        assert!(prof.approval_answer(screen, "1").is_err());
        // Two adjacent echoes still are not a menu: neither is the
        // indented highlight a real option block needs.
        let screen = "❯ first prompt\n❯ second prompt\n  Do you want to proceed?\n❯ \n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        assert!(prof.approval_answer(screen, "1").is_err());
    }

    #[test]
    fn indented_echo_with_a_sibling_is_not_a_menu() {
        // Round-4 review: an *indented* transcript `❯` passes sel_row,
        // and a transcript row landing at the same column reads as its
        // sibling — the two-row block used to set `approval_menu` and
        // `answer` then keyed a live input box. Indentation alone never
        // corroborates: only a numbered option inside the run or the
        // anchored legend directly below does.
        let screen = "  Do you want to proceed? — I'll ask first.\n    ❯ npm run dev\n      vite v5 ready\n\n❯ \n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        let prof = profile();
        let err = prof.approval_answer(screen, "2").unwrap_err();
        assert!(err.to_string().contains("indentation only"), "{err}");
    }

    #[test]
    fn legend_below_the_block_corroborates_it() {
        // The same two-row block WITH the menu legend directly below
        // is a real select — corroboration is what separates it from
        // transcript text.
        let screen = " ❯ npm run dev\n   vite v5 ready\n\n Enter to confirm · Esc to cancel\n";
        let p = analyze_claude(screen, None);
        assert!(p.approval_menu, "{p:?}");
        let prof = profile();
        assert_eq!(
            prof.approval_answer(screen, "2").unwrap(),
            vec!["Down", "Enter"]
        );
    }

    #[test]
    fn lone_legend_far_from_the_anchor_is_not_a_menu() {
        // A `cat`ed doc can quote an anchor and a hint row far apart —
        // one hint only corroborates beside the anchor it belongs to
        // (within two rows); a stray `Esc to cancel` in scrollback is
        // text, never a menu.
        let screen = " Do you want to proceed?\n unrelated output row\n another row\n and one more\n Esc to cancel\n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        // …but the hint beside its anchor is a real menu's legend.
        let screen = " Do you want to proceed?\n Esc to cancel\n ❯ 1. Yes\n   2. No\n";
        let p = analyze_claude(screen, None);
        assert!(p.approval_menu, "{p:?}");
    }

    #[test]
    fn quoted_menu_above_the_live_input_box_is_not_a_menu() {
        // Round-5 review: corroboration is screen-local text, so an
        // agent quoting the pane verbatim satisfies every textual
        // check — anchor, a `❯`-led numbered run, the works. What the
        // quote cannot fake is the frame: a live menu replaces the
        // input box's interior, so a boxed `❯` prompt below the block
        // proves the block is transcript. `answer` must refuse too —
        // a digit keyed here lands in a live input line.
        let screen = "● I reproduced it. The pane printed:\n\n    Do you want to proceed?\n    ❯ 1. Yes\n      2. No, and tell Claude what to do differently\n\n  So it is waiting on you.\n────────────────────\n❯ \n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        assert!(p.idle, "{p:?}");
        let prof = profile();
        assert!(prof.approval_answer(screen, "2").is_err());
        // The same quote with the input box still populated — a draft
        // under the quote must stay a draft, never a menu.
        let screen = "● the pane printed:\n    Do you want to proceed?\n    ❯ 1. Yes\n      2. No\n────────────────────\n❯ half-typed reply\n";
        let p = analyze_claude(screen, None);
        assert!(!p.approval_menu && p.input_nonempty, "{p:?}");
        assert!(prof.approval_answer(screen, "1").is_err());
    }

    /// CAD-310: the pane scrub keeps the agent's identity and the
    /// daemon's tracker and profile; test overrides and nested-session
    /// variables still go.
    #[test]
    fn pane_scrub_keeps_the_daemon_tracker_and_profile() {
        let names = scrub_names(
            [
                "CADENCE_ALIAS",
                "CADENCE_STATE_DIR",
                "CADENCE_PM_DIR",
                "CADENCE_PROFILE",
                "CADENCE_CLAUDE_COMMAND",
                "CLAUDE_PID",
                "PATH",
            ]
            .into_iter()
            .map(str::to_string),
        );
        for kept in [
            "CADENCE_ALIAS",
            "CADENCE_STATE_DIR",
            "CADENCE_PM_DIR",
            "CADENCE_PROFILE",
            "PATH",
        ] {
            assert!(
                !names.iter().any(|n| n == kept),
                "{kept} scrubbed: {names:?}"
            );
        }
        for gone in ["CADENCE_CLAUDE_COMMAND", "CLAUDE_PID"] {
            assert!(names.iter().any(|n| n == gone), "{gone} kept: {names:?}");
        }
    }
}
