//! The Cursor Agent TUI profile (`cursor-agent`): screen signatures,
//! the `analyze` reduction, launch argv, and chat ownership proven
//! through the chat store the TUI keeps open.
//!
//! Cursor has no session-lock file or pid registry. What the running
//! TUI does expose is the chat's own state: it holds an open file
//! descriptor on `~/.cursor/chats/<project-hash>/<chat-id>/store.db`
//! for the whole session (observed live on 2026.09.15 and
//! 2026.09.18), and the pane's `cursor-agent` process carries the
//! chat id element-wise in its argv (`--resume <chatId>`). Ownership
//! is *proven*, not assumed: a chat whose store fd or `--resume` argv
//! belongs to a process outside our pane means another TUI owns it —
//! open refuses rather than taking it over, and a changed owner fails
//! closed.
//!
//! A fresh launch mints its chat id first (`cursor-agent
//! create-chat`) and opens it with `--resume`, so every pane — fresh
//! or resumed — carries its chat id in argv from exec.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use crate::adapter::{Probe, ProviderEnv};
use crate::error::{Error, Result};
use crate::proc::run_bounded;
use crate::store::Agent;

use super::profile::TuiProfile;
use super::{descends_from, resolve_on_path, shlex_quote};

/// Bounded wait for the launched Cursor TUI to open its chat store —
/// a cold node start plus the first `store.db` open can take a while.
const OPEN_DEADLINE: Duration = Duration::from_secs(45);
/// Bound on `cursor-agent create-chat` — a network round trip that
/// must never stall the actor's open path.
const MINT_DEADLINE: Duration = Duration::from_secs(20);
/// How much of the screen bottom counts as the status region: input
/// line, status chips, model/cwd bar and a menu tall enough for
/// Cursor's permission select. Approval markers only match inside it
/// — the transcript above can legitimately show these strings as
/// text. (Busy is anchored tighter still: the input line's interrupt
/// hint or the status row directly above it — see `analyze_cursor`.)
const STATUS_LINES: usize = 16;
/// The wider window a menu may occupy: the permission select (~8
/// rows) plus the input box, status chips and model/cwd bar that can
/// stay visible below it — its own chrome can sit above the busy
/// anchor. Menu evidence only matches inside it.
const MENU_LINES: usize = 24;

/// Cursor TUI screen signatures — THE one place they live. A provider
/// TUI update means editing this table, never the gate logic. Every
/// string is verbatim from a live pane capture (2026.09.15-d2fe57e;
/// the shapes held unchanged on 2026.09.18-9a7762b).
mod cursor_screen {
    /// Glyph leading the input line (also the selected option of an
    /// open menu — approval is checked before input parsing).
    pub const PROMPT: &str = "→";
    /// Interrupt hint the TUI paints at the right edge of the input
    /// line while a turn runs — busy evidence on the input row itself.
    pub const INTERRUPT: &str = "ctrl+c to stop";
    /// Input watermarks that mean EMPTY input — the TUI renders them
    /// dim inside the line, and after a turn the placeholder flips to
    /// the follow-up form. Presence of either (exactly) is an empty
    /// input, never a staged draft.
    pub const PLACEHOLDERS: &[&str] = &[
        "Plan, search, build anything",
        "Add a follow-up",
        "Ask anything",
    ];
    /// Spinner words on the status row directly above the input while
    /// a turn runs — `<braille> <word>  N tokens`.
    pub const SPINNER: &[&str] = &[
        "Running",
        "Working",
        "Thinking",
        "Reading",
        "Writing",
        "Searching",
        "Planning",
    ];
    /// Staged-queue rows while busy — the `┌─ follow-ups ─┐` box's
    /// title and its `enter steer · ↑ select/edit · esc cancel`
    /// footer (observed when a message was typed mid-turn).
    pub const QUEUED: &[&str] = &["follow-ups", "enter steer", "select/edit"];
    /// Rows skipped walking up from the input line: a `Tip:` hint is
    /// neutral documentation (it can sit between the spinner and the
    /// box), and box rules are frame, not status.
    pub const TIP: &str = "Tip:";
    /// Strong approval evidence — strings only a real permission box
    /// (or a pending inline approval) renders. Anchors alone decide;
    /// transcript text can legitimately quote the hints below, so they
    /// never decide by themselves.
    pub const ANCHOR: &[&str] = &[
        "Run this command?",
        "Not in allowlist",
        "Waiting for approval",
    ];
    /// Secondary menu chrome — the approval box's options and key
    /// hints plus the generic select footers (`/`, `@`, `/skills`
    /// pickers). A cluster of two or more in the status region counts
    /// as a menu; a lone hint is transcript-speakable and does not.
    pub const HINT: &[&str] = &[
        "to allowlist? (tab)",
        "Run (once) (y)",
        "Run Everything (shift+tab)",
        "tell the agent what to do instead",
        "to navigate",
        "more below",
        "Esc to close",
    ];
}

/// Leading characters Cursor's TUI interprets before the prompt text —
/// observed live in a scratch pane (CAD-56): `/` opens the command
/// menu, `!` switches to shell-command mode, `@` opens the file
/// picker. `#` stays a literal draft and is deliberately absent.
pub const FORBIDDEN_PREFIXES: &[char] = &['/', '!', '@'];

/// Write `contents` to `path` atomically: a same-directory temp file,
/// fsync, then rename — a crash mid-write leaves the original bytes
/// (or a `.cadence-tmp.*` leftover the next merge's unique name never
/// collides with), never a torn config. The temp name carries pid +
/// uuid so two concurrent merges sharing the config cannot truncate
/// and rename each other's temp into a torn file. `mode` is applied
/// after creation so the auth-carrying file is never umask-masked or
/// born world-readable.
fn write_atomic(path: &Path, contents: &str, mode: u32) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let tmp = PathBuf::from(format!(
        "{}.cadence-tmp.{}.{}",
        path.display(),
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&tmp)
        .map_err(|e| Error::provider(format!("cannot write {}: {e}", tmp.display())))?;
    // mode() is umask-masked at creation — apply it explicitly.
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    if let Err(e) = f.write_all(contents.as_bytes()).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::provider(format!(
            "cannot write {}: {e}",
            tmp.display()
        )));
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| Error::provider(format!("cannot replace {}: {e}", path.display())))
}

/// Write `.bak` holding the original bytes, created with the source
/// file's mode from the start — the config carries auth details, so
/// write-then-chmod would leave a world-readable window.
fn write_backup(path: &Path, original: &str, mode: u32) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let bak = PathBuf::from(format!("{}.bak", path.display()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(&bak)
        .map_err(|e| Error::provider(format!("cannot write {}: {e}", bak.display())))?;
    f.write_all(original.as_bytes())
        .map_err(|e| Error::provider(format!("cannot write {}: {e}", bak.display())))?;
    // A `.bak` that already existed keeps its old mode through
    // create+truncate — normalize it to the source's.
    let _ = std::fs::set_permissions(&bak, std::fs::Permissions::from_mode(mode));
    Ok(())
}

/// A `cli-config.json` shape we refuse to silently rewrite — the file
/// parses but a node we must edit holds an unexpected type.
fn malformed(path: &Path, what: &str) -> Error {
    Error::provider(format!(
        "{} is malformed ({what}) — refusing to launch cursor-agent; \
         fix or remove the file",
        path.display()
    ))
}

/// A chat id's shape: uuid groups 8-4-4-4-12, hex only. `create-chat`
/// output may carry warnings around the id — only this shape counts.
fn uuid_shape(token: &str) -> bool {
    const GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
    let parts: Vec<&str> = token.split('-').collect();
    parts.len() == 5
        && GROUPS
            .iter()
            .zip(parts.iter())
            .all(|(n, part)| part.len() == *n && part.chars().all(|c| c.is_ascii_hexdigit()))
}

/// A row of the TUI's own activity indicator: a braille spinner glyph
/// (U+2800–U+28FF — the TUI animates through them) or a spinner word
/// with its live token counter.
fn spinner_row(row: &str) -> bool {
    row.chars().any(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
        || (cursor_screen::SPINNER.iter().any(|w| row.contains(w)) && row.contains("tokens"))
}

/// Text staged in the input row. The interrupt hint shares that row,
/// right-aligned, and `capture-pane` pads the row to the pane width
/// *after* the hint — so the hint is not a suffix. `trim_end_matches`
/// then leaves `ctrl+c to stop` in the draft and a drained
/// `Add a follow-up` watermark reads as unsubmitted text (CAD-612).
/// Strip the hint wherever it sits, then the padding; what remains is
/// the placeholder or a real draft.
fn input_draft(input_line: &str) -> String {
    let trimmed = input_line.trim();
    let body = match trimmed.find(cursor_screen::INTERRUPT) {
        Some(at) => trimmed[..at].trim_end(),
        None => trimmed,
    };
    body.trim_start_matches(cursor_screen::PROMPT)
        .trim()
        .to_string()
}

/// Reduce a captured Cursor screen to gate facts. The input line is
/// the last `→`-leading row inside the bottom status region — and it
/// is never the last non-blank row (the model/cwd bar always sits
/// below it), so a `→` higher up or a bare `→` ending the frame is
/// transcript text (a menu cursor, a pasted glyph, a scrolled-out
/// input row), never the prompt. Text after a real `→` that is not an
/// input watermark is a staged draft. Menus and busy markers win over
/// prompt parsing — a `→` leads the first approval option too.
/// Approval needs a strong anchor (`Run this command?`,
/// `Not in allowlist`, `Waiting for approval`) or a cluster of menu
/// hints — the transcript above can legitimately print a lone hint,
/// and it all only matches inside the status region. Busy is anchored
/// tighter still (CAD-50): the `ctrl+c to stop` hint on the input
/// line itself, or the status row directly above it — the braille
/// spinner, a spinner word with its token count, or the staged
/// follow-ups box. Blank rows, box rules and `Tip:` rows sit between
/// the two and are skipped — a tip quoting the same hints stays
/// neutral. The region is anchored at the last NON-BLANK row —
/// `capture-pane` pads to pane height, so a young session on a tall
/// pane has blank rows below the real content.
/// A menu option row: the `→`-led selected row (never a placeholder —
/// the input box's `→` watermarks are not options, and a bare `→` is
/// transcript text) or a row carrying a `(hint)` hotkey suffix like
/// `Add Shell(whoami) to allowlist? (tab)`.
fn opt_row(l: &str) -> bool {
    let t = l.trim_start();
    if let Some(rest) = t.strip_prefix('→') {
        let rest = rest.trim();
        !rest.is_empty() && !cursor_screen::PLACEHOLDERS.contains(&rest)
    } else {
        // A bare option row must carry a real menu key in its trailing
        // `(hint)` — transcript text like `foo(bar)` would otherwise
        // satisfy the shape and fake an option block under a `→` row.
        hotkey(l).is_some()
    }
}

/// A footer/legend row matching hint `h` — anchored on the trimmed
/// row's leading glyph, a legend word, or an option row carrying the
/// hint text, so a transcript quoting the same words mid-line does
/// not count.
fn hint_row(l: &str, h: &str) -> bool {
    if !l.contains(h) {
        return false;
    }
    let t = l.trim_start();
    t.starts_with(h)
        || t.starts_with('↑')
        || t.starts_with('↓')
        || t.starts_with('↵')
        || t.starts_with("Esc")
        || opt_row(l)
}

/// An option row that is not the highlighted one: never `→`-led (a
/// menu highlights exactly one row — a second `→` line is a transcript
/// echo, not a sibling) and always carrying its `(key)` hint, so a
/// bare transcript row cannot qualify.
fn keyed_row(l: &str) -> bool {
    !l.trim_start().starts_with('→') && hotkey(l).is_some()
}

/// Any menu-chrome row — a keyed sibling option or an anchored hint.
/// A `→`-led row is deliberately not chrome: transcript echoes lead
/// with `→` too, and only real structure may support a menu.
fn chrome_row(l: &str) -> bool {
    keyed_row(l) || cursor_screen::HINT.iter().any(|h| hint_row(l, h))
}

/// The input row's index under the prompt rule — a `→`-led row that is
/// never the frame's last row (the model/cwd bar renders below it) and
/// always inside the status region. The row directly below is never
/// menu chrome: the input sits on the model/cwd bar, while a menu's
/// highlighted option sits on keyed sibling rows. Menu parsing must
/// never treat the input as an option row: a keyed row beside it
/// cannot make it a highlight.
fn prompt_row(lines: &[&str]) -> Option<usize> {
    // Search `→` rows bottom-up, not just the last one — when the
    // lowest `→` is a menu highlight (chrome below it) the input row
    // is still the next `→` up that carries the input frame. A `→`
    // row that fails the frame test is a menu row or a transcript
    // echo, never the input.
    lines
        .iter()
        .enumerate()
        .rev()
        .find(|(i, l)| {
            l.trim_start().starts_with(cursor_screen::PROMPT)
                && *i + 1 < lines.len()
                && lines.len() - i <= STATUS_LINES
                && !chrome_row(lines[i + 1])
        })
        .map(|(i, _)| i)
}

/// The open option block as `(selected row, block start, block end)`.
/// The selected row is the last `→`-led row with menu chrome directly
/// below it — or the frame's last row with a keyed option directly
/// above it (a highlighted final option has nothing below it) — while
/// the input box's `→` is followed by the model/cwd bar and is never
/// eligible (`prompt` names its row). The block then extends over
/// contiguous keyed rows up AND down: options printed above the
/// highlighted row are part of the menu, so a choice counts the whole
/// list in printed order, never just the suffix from `→`.
fn menu_block(lines: &[&str], prompt: Option<usize>) -> Option<(usize, usize, usize)> {
    let sel = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(i, l)| {
            Some(*i) != prompt
                && opt_row(l)
                && l.trim_start().starts_with('→')
                && (lines[i + 1..]
                    .iter()
                    .find(|n| !n.trim().is_empty())
                    .is_some_and(|n| chrome_row(n))
                    // The keyed-above path (a highlight on the frame's
                    // last row) needs the `→` row's own `(key)` — an
                    // unidentified input row has a keyed transcript row
                    // above it too, but a draft never carries a hotkey.
                    || (*i > 0 && keyed_row(lines[i - 1]) && hotkey(l).is_some()))
        })
        .map(|(i, _)| i)?;
    let mut start = sel;
    while start > 0 && keyed_row(lines[start - 1]) {
        start -= 1;
    }
    let mut end = sel;
    while end + 1 < lines.len() && keyed_row(lines[end + 1]) {
        end += 1;
    }
    // A live menu renders in place of the input row — a menu-looking
    // block sitting ABOVE a still-live `→` input line is a transcript
    // quoting the pane verbatim, and `answer` would key the choice
    // into the draft. The frame position is the corroboration the
    // text alone cannot supply.
    if prompt.is_some_and(|p| end < p) {
        return None;
    }
    Some((sel, start, end))
}

/// The option's `(hint)` hotkey as a tmux key name, from a fixed
/// allowlist — the named keys Cursor prints plus the observed single
/// characters `y`/`n`, and only when the `(key)` ends the row AND is
/// whitespace-separated from the label: `take(n)` is a function call,
/// `Ok(v)`/`f(1)`/`(C-c)` are transcript text wearing the shape — not
/// a menu key: the answer falls back to arrows + Enter instead.
fn hotkey(row: &str) -> Option<String> {
    let inner = row.trim_end().strip_suffix(')')?;
    let (label, hint) = inner.rsplit_once('(')?;
    if !label.ends_with(char::is_whitespace) {
        return None;
    }
    let hint = hint.trim();
    // `(esc or n)` lists alternatives — take the last.
    let hint = hint.rsplit(" or ").next().unwrap_or(hint);
    match hint {
        "tab" => Some("Tab".to_string()),
        "shift+tab" => Some("BTab".to_string()),
        "enter" => Some("Enter".to_string()),
        "esc" => Some("Escape".to_string()),
        "space" => Some("Space".to_string()),
        "y" | "n" => Some(hint.to_string()),
        _ => None,
    }
}

/// The line naming what the menu asks: the furthest row of the menu
/// block above the option list — ` $ whoami in .` names the command
/// being approved.
fn menu_subject(screen: &str) -> Option<String> {
    let lines: Vec<&str> = screen.trim_end().lines().collect();
    let menu_top = lines.len().saturating_sub(MENU_LINES);
    let (_, start, _) = menu_block(&lines[menu_top..], prompt_row(&lines[menu_top..]))?;
    let start = start + menu_top;
    // Walk up over non-blank rows and single blank gaps; a double
    // blank or a border row ends the menu's block.
    let mut subject: Option<String> = None;
    let mut blanks = 0;
    for l in lines[..start].iter().rev() {
        let t = l.trim();
        if t.is_empty() {
            blanks += 1;
            if blanks > 1 {
                break;
            }
            continue;
        }
        if t.chars().filter(|c| matches!(c, '─' | '═')).count() >= 8 {
            break;
        }
        subject = Some(t.split_whitespace().collect::<Vec<_>>().join(" "));
        blanks = 0;
    }
    subject.map(|s| s.chars().take(100).collect())
}

pub fn analyze_cursor(screen: &str, _cursor: Option<(u32, u32)>) -> Probe {
    let content = screen.trim_end();
    // Menu evidence lives in the wider menu window — the permission
    // select plus still-visible status chrome can push its rows above
    // the busy anchor. Natural-language anchors are ordinary sentence
    // text, so even matched on the trimmed row's leading text they
    // decide only beside real menu structure: a parsed option row or
    // the anchored legend — an indented transcript line quoting
    // `Run this command?` stays inert. `Waiting for approval` sits
    // mid-row on the `$ cmd …` status line, same rule. A hint cluster
    // without an anchor still needs the `→` option block.
    let menu_lines: Vec<&str> = content
        .lines()
        .rev()
        .take(MENU_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let anchor = menu_lines.iter().any(|l| {
        let t = l.trim_start();
        cursor_screen::ANCHOR
            .iter()
            .filter(|a| **a != "Waiting for approval")
            .any(|a| t.starts_with(a))
    });
    let waiting = menu_lines
        .iter()
        .any(|l| l.trim_start().starts_with('$') && l.contains("Waiting for approval"));
    let hints = cursor_screen::HINT
        .iter()
        .filter(|h| menu_lines.iter().any(|l| hint_row(l, h)))
        .count();
    // The input row is never menu evidence — a keyed sibling beside it
    // cannot make it the highlighted option.
    let block = menu_block(&menu_lines, prompt_row(&menu_lines));
    // Structure beside an anchor means a qualified option block or two
    // distinct hint rows — one hint alone is transcript-speakable, and
    // a lone `→` row is the input box or a transcript echo, never menu
    // evidence by itself.
    let structure = block.is_some() || hints >= 2;
    let approval_menu = ((anchor || waiting) && structure) || (block.is_some() && hints >= 2);
    let lines: Vec<&str> = content.lines().collect();
    // Prompt search is anchored to the bottom `STATUS_LINES` rows —
    // the input row lives there on every real frame — and can never
    // be the last row: the model/cwd bar always renders below it.
    let prompt_idx = prompt_row(&lines);
    let prompt_visible = prompt_idx.is_some();
    let input_line = prompt_idx.map(|i| lines[i]).unwrap_or("");
    // The interrupt hint shares the input row while a turn runs —
    // busy evidence on the input line itself, and never part of the
    // staged text.
    let interrupt_hint = input_line.contains(cursor_screen::INTERRUPT);
    let draft = input_draft(input_line);
    // Busy is decided by positive evidence tied to the input line:
    // the interrupt hint on it, or the status row directly above —
    // the first row up that is not blank, a box rule, or a `Tip:`
    // hint. A transcript echo cannot land there while the pane is
    // idle.
    let status_row = prompt_idx.and_then(|i| {
        lines[..i].iter().rev().find(|l| {
            let t = l.trim();
            !t.is_empty()
                && !t.starts_with(cursor_screen::TIP)
                && t.chars().filter(|c| matches!(c, '─' | '═')).count() < 8
        })
    });
    let status_busy = status_row.is_some_and(|row| {
        spinner_row(row) || cursor_screen::QUEUED.iter().any(|m| row.contains(m))
    });
    let busy_marker = interrupt_hint || status_busy;
    let input_nonempty =
        !draft.is_empty() && !cursor_screen::PLACEHOLDERS.contains(&draft.as_str());
    let (idle, reason) = if approval_menu {
        (
            false,
            menu_subject(content).unwrap_or_else(|| "approval menu is open".to_string()),
        )
    } else if interrupt_hint {
        (
            false,
            "tui is busy (interrupt hint on the input line)".to_string(),
        )
    } else if status_busy {
        (
            false,
            "tui is busy (status row above the input line)".to_string(),
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

/// The chat id a `/proc/<pid>` process is attached to, proven by an
/// open fd on `<chats>/<project-hash>/<chat-id>/store.db` — the file
/// the TUI holds for the session's whole life. SQLite sidecars
/// (`store.db-wal`, `store.db-journal`) name the same chat.
fn store_db_chat(chats_dir: &Path, pid: u32) -> Option<String> {
    // Trailing slashes in a configured chats dir must not double the
    // separator in the fd-link prefix.
    let prefix = format!("{}/", chats_dir.to_string_lossy().trim_end_matches('/'));
    let fds = std::fs::read_dir(format!("/proc/{pid}/fd")).ok()?;
    for fd in fds.flatten() {
        let Ok(link) = std::fs::read_link(fd.path()) else {
            continue;
        };
        let link = link.to_string_lossy().into_owned();
        let Some(rest) = link.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = rest.rsplit('/');
        if parts
            .next()
            .is_some_and(|name| name.starts_with("store.db"))
        {
            if let Some(chat) = parts.next() {
                return Some(chat.to_string());
            }
        }
    }
    None
}

/// The chat id a cursor-agent argv claims, proven element-wise:
/// `--resume` followed by the id. Only argv[0]s that name the real
/// binary count — a foreign process's `--resume` is its own flag.
fn argv_chat(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> = raw
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    if !args.first().is_some_and(|a| a.contains("cursor-agent")) {
        return None;
    }
    let i = args.iter().position(|a| a == "--resume")?;
    Some(args.get(i + 1)?.clone())
}

/// The entry that lets a Cursor worker run `cadence …` commands —
/// `cadence self`, `message result`, `agent list` — without an
/// approval prompt in every permission mode. Cursor has no launch
/// flag for allowed tools, so it rides the CLI's own
/// `permissions.allow` in `cli-config.json` (entries are command
/// prefixes: `Shell(cadence)` covers every `cadence` invocation).
const CADENCE_ALLOW_ENTRY: &str = "Shell(cadence)";

/// The Cursor profile: `cursor-agent` argv (`create-chat` mint +
/// `--resume <chat>` on every launch, `--trust` always, `--model`,
/// and `--force`/`--auto-review` permission modes), chat ownership
/// via the open `store.db` fd / `--resume` argv, and the Cursor
/// screen analyzer.
pub struct CursorProfile {
    /// `~/.cursor/chats` — overridable in tests.
    chats_dir: PathBuf,
    /// Launch command prefix: the `CADENCE_CURSOR_COMMAND` override
    /// verbatim, else a PATH-resolved `cursor-agent`.
    command: String,
    model: Option<String>,
    /// `auto-review` → `--auto-review`, `force` → `--force`; unset
    /// keeps Cursor's own default permission behavior.
    permission_mode: Option<String>,
}

impl CursorProfile {
    /// Resolve the profile from the agent's stored params and the
    /// environment: the chats dir (`CADENCE_CURSOR_CHATS` for tests),
    /// the launch command (`CADENCE_CURSOR_COMMAND` used verbatim —
    /// tests pass `python3 mock.py <chats>` — else a `cursor-agent`
    /// found on PATH), and the model/permission params replayed on
    /// every launch exactly as registered.
    pub fn new(agent: &Agent, env: &ProviderEnv) -> Result<Self> {
        let chats_dir = match env.var("CADENCE_CURSOR_CHATS") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => match std::env::var("HOME") {
                Ok(home) if !home.is_empty() => PathBuf::from(home).join(".cursor/chats"),
                // A relative `.cursor/chats` would silently follow the
                // daemon's cwd — refuse rather than scan the wrong tree.
                _ => {
                    return Err(Error::provider(
                        "HOME is unset and CADENCE_CURSOR_CHATS is not — \
                         cannot locate ~/.cursor",
                    ))
                }
            },
        };
        let command = match env.var("CADENCE_CURSOR_COMMAND") {
            Some(cmd) if !cmd.is_empty() => cmd,
            _ => resolve_on_path("cursor-agent").map(|p| shlex_quote(&p))?,
        };
        let params = agent.params.clone().unwrap_or(Value::Null);
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.model.clone());
        let permission_mode = params
            .get("permission_mode")
            .and_then(Value::as_str)
            .map(str::to_string);
        Ok(Self {
            chats_dir,
            command,
            model,
            permission_mode,
        })
    }

    /// Mint a fresh chat id — `cursor-agent create-chat` prints it on
    /// stdout. The id is unbound to any workspace until the TUI opens
    /// it, so the daemon's own cwd is fine (observed: an id minted in
    /// one directory opens cleanly in another).
    fn mint_chat(&self) -> Result<String> {
        let out = run_bounded(
            Command::new("sh").args(["-c", &format!("{} create-chat", self.command)]),
            MINT_DEADLINE,
        )
        .map_err(|e| Error::provider(format!("cursor-agent create-chat failed: {e}")))?;
        if !out.status.success() {
            return Err(Error::provider(format!(
                "cursor-agent create-chat failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Anchor on the id's real shape — uuid groups 8-4-4-4-12 —
        // and fail closed on ambiguity: a warning line's trailing
        // token or two printed ids must never be launched as a
        // session.
        let found: std::collections::BTreeSet<&str> = stdout
            .split_whitespace()
            .filter(|token| uuid_shape(token))
            .collect();
        match found.len() {
            1 => Ok(found.into_iter().next().unwrap().to_string()),
            0 => Err(Error::provider(format!(
                "cursor-agent create-chat printed no chat id: {}",
                stdout.trim()
            ))),
            _ => Err(Error::provider(format!(
                "cursor-agent create-chat printed ambiguous chat ids: {}",
                stdout.trim()
            ))),
        }
    }

    /// `cli-config.json` sits beside `chats/` under `~/.cursor` — the
    /// same root override (`CADENCE_CURSOR_CHATS`) relocates it in
    /// tests, so the merge never touches a real config there.
    fn cli_config_path(&self) -> PathBuf {
        self.chats_dir
            .parent()
            .unwrap_or(self.chats_dir.as_path())
            .join("cli-config.json")
    }

    /// Idempotent `Shell(cadence)` merge into the CLI's allowlist,
    /// run at every launch: absent entry → write, present → leave the
    /// file byte-identical (no `.bak`, no rewrite). The file is read
    /// as JSON first — an unparseable config refuses the launch
    /// rather than clobbering the user's settings, and a non-array
    /// `allow` is malformed the same way. A `.bak` of the original
    /// bytes is written before every modification.
    fn ensure_cadence_allowlist(&self) -> Result<()> {
        self.merge_cadence_allowlist(&|| self.global_write_gate())
    }

    /// A sandbox shares $HOME with production: writing the real
    /// ~/.cursor config needs the operator's opt-in (CAD-310). A
    /// relocated root (`CADENCE_CURSOR_CHATS`) is the caller's own.
    fn global_write_gate(&self) -> Result<()> {
        let real = std::env::var_os("HOME")
            .is_some_and(|home| self.chats_dir == Path::new(&home).join(".cursor/chats"));
        if real {
            crate::sandbox::refuse_global_unless_allowed(
                "the Cursor `Shell(cadence)` merge into ~/.cursor/cli-config.json",
            )?;
        }
        Ok(())
    }

    /// The merge [`Self::ensure_cadence_allowlist`] runs. `gate` is
    /// asked only when the merge is about to write: a config that
    /// already allows `Shell(cadence)` launches with no question.
    fn merge_cadence_allowlist(&self, gate: &dyn Fn() -> Result<()>) -> Result<()> {
        let path = self.cli_config_path();
        // A symlinked config must be written through, never replaced:
        // resolve the link so `.bak` and the temp+rename land beside
        // the target and the link itself survives. `read_link` also
        // covers a dangling link — writing the (missing) target is
        // what an ordinary write through the link would have done.
        let write_path = match std::fs::canonicalize(&path) {
            Ok(resolved) => resolved,
            Err(_) => match std::fs::read_link(&path) {
                Ok(target) if target.is_absolute() => target,
                Ok(target) => path.parent().unwrap_or_else(|| Path::new(".")).join(target),
                Err(_) => path.clone(),
            },
        };
        let original = match std::fs::read_to_string(&write_path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(Error::provider(format!(
                    "cannot read {}: {e}",
                    path.display()
                )))
            }
        };
        let mut doc: Value = match &original {
            Some(text) => serde_json::from_str(text).map_err(|e| {
                Error::provider(format!(
                    "{} is not valid JSON ({e}) — refusing to launch \
                     cursor-agent; fix or remove the file",
                    path.display()
                ))
            })?,
            None => serde_json::json!({"version": 1}),
        };
        let root = doc
            .as_object_mut()
            .ok_or_else(|| malformed(&path, "config root is not a JSON object"))?;
        let permissions = match root.get_mut("permissions") {
            Some(v) => v
                .as_object_mut()
                .ok_or_else(|| malformed(&path, "`permissions` is not a JSON object"))?,
            None => {
                root.insert(
                    "permissions".to_string(),
                    serde_json::json!({"allow": [], "deny": []}),
                );
                root.get_mut("permissions")
                    .and_then(Value::as_object_mut)
                    .unwrap()
            }
        };
        let allow = match permissions.get_mut("allow") {
            Some(v) => v
                .as_array_mut()
                .ok_or_else(|| malformed(&path, "`permissions.allow` is not an array"))?,
            None => {
                permissions.insert("allow".to_string(), serde_json::json!([]));
                permissions
                    .get_mut("allow")
                    .and_then(Value::as_array_mut)
                    .unwrap()
            }
        };
        if allow
            .iter()
            .any(|e| e.as_str() == Some(CADENCE_ALLOW_ENTRY))
        {
            return Ok(());
        }
        gate()?;
        allow.push(Value::String(CADENCE_ALLOW_ENTRY.to_string()));
        let rendered = serde_json::to_string_pretty(&doc)?;
        // The new file keeps the source's mode; a brand-new config is
        // born owner-only (it will hold the same auth-adjacent data).
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&write_path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        if let Some(text) = &original {
            write_backup(&write_path, text, mode)?;
        } else if let Some(parent) = write_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| Error::provider(format!("cannot create {}: {e}", parent.display())))?;
        }
        write_atomic(&write_path, &format!("{rendered}\n"), mode)
    }

    /// Every live (pid, chat) attachment on the host: the open
    /// `store.db` fd the TUI holds, or a cursor-agent argv carrying
    /// `--resume <chat>`. Single-shot: callers that need resilience
    /// retry at their own decision point.
    fn attachments(&self) -> Vec<(u32, String)> {
        let mut out = Vec::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return out;
        };
        for proc in procs.flatten() {
            let Ok(pid) = proc.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Some(chat) = store_db_chat(&self.chats_dir, pid).or_else(|| argv_chat(pid)) {
                out.push((pid, chat));
            }
        }
        out
    }
}

impl TuiProfile for CursorProfile {
    fn name(&self) -> &'static str {
        "Cursor"
    }

    /// A fresh open mints its chat id here — once, before the pane
    /// exists. The adapter folds it into `params.session`
    /// (`cadence/session_minted`) so a later respawn resumes it; an
    /// id minted but never proven is still the agent's session. The
    /// allowlist merge runs first: it is the check that can refuse
    /// the launch, and a mint is a 20 s network round trip — never
    /// spend one on a config that will fail the launch anyway.
    fn prepare_session(&self) -> Result<Option<String>> {
        self.ensure_cadence_allowlist()?;
        self.mint_chat().map(Some)
    }

    /// `cursor-agent --trust [--model M] [--force|--auto-review]
    /// --resume <chat>` — every launch carries its chat id in argv
    /// from exec (fresh ids arrive pre-minted via `prepare_session`).
    /// `--trust` keeps the workspace-trust prompt from ever gating
    /// the pane; Cadence manages worktrees itself, so cursor's own
    /// `--worktree` is never used. The allowlist merge runs here too —
    /// the resume path skips `prepare_session`, and a malformed
    /// `cli-config.json` must still refuse before the pane exists.
    fn launch_command(&self, resume: Option<&str>) -> Result<String> {
        self.ensure_cadence_allowlist()?;
        let chat = resume.ok_or_else(|| {
            Error::internal("cursor-agent needs a chat id — minted in prepare_session")
        })?;
        let mut argv = format!("{} --trust", self.command);
        if let Some(model) = &self.model {
            argv.push_str(&format!(" --model {}", shlex_quote(model)));
        }
        match self.permission_mode.as_deref() {
            Some("force") => argv.push_str(" --force"),
            Some("auto-review") => argv.push_str(" --auto-review"),
            _ => {}
        }
        argv.push_str(&format!(" --resume {}", shlex_quote(chat)));
        Ok(argv)
    }

    fn exit_banner(&self) -> &'static str {
        "Cursor exited. This pane will close."
    }

    /// The chat whose live attachment descends from `pane_pid`, or
    /// `None` when the pane owns no chat.
    fn owned_session(&self, pane_pid: u32) -> Option<String> {
        self.attachments()
            .into_iter()
            .find(|(pid, _)| descends_from(*pid, pane_pid))
            .map(|(_, chat)| chat)
    }

    /// Adopt the pane's owned chat only when a stored id already names
    /// it — a wanted chat adopts back, a changed owner fails closed.
    /// With no desired id there is nothing to adopt: cursor chats are
    /// disposable, so an unrecorded pane session is refused and the
    /// next open mints a fresh one instead.
    fn resolve_session(&self, desired: Option<&str>, found: Option<String>) -> Result<String> {
        match (desired, found) {
            (Some(want), Some(found)) if *want == found => Ok(found),
            (Some(want), Some(found)) => Err(Error::provider(format!(
                "pane owns chat '{found}', expected '{want}' — \
                 changed owner fails closed"
            ))),
            (Some(want), None) => Err(Error::provider(format!(
                "pane has no live Cursor chat for '{want}'"
            ))),
            (None, Some(found)) => Err(Error::provider(format!(
                "pane owns chat '{found}' but no chat was recorded — \
                 refusing to adopt an unrecorded session"
            ))),
            (None, None) => Err(Error::provider("pane exists but owns no Cursor chat")),
        }
    }

    /// Cursor chats are disposable mintable ids — when a stored chat
    /// provably cannot resume, the daemon may drop it and let the next
    /// open mint a fresh one.
    fn session_is_disposable(&self) -> bool {
        true
    }

    /// Refuse takeover: a live attachment to `session` outside our
    /// (future) pane means another TUI already owns it.
    fn refuse_takeover(&self, session: &str) -> Result<()> {
        if let Some((pid, _)) = self
            .attachments()
            .into_iter()
            .find(|(_, chat)| chat.as_str() == session)
        {
            return Err(Error::rejected(format!(
                "Cursor chat '{session}' is owned by another terminal \
                 (pid {pid}); close it first — no takeover"
            )));
        }
        Ok(())
    }

    /// The pane owns `native` while a descendant of its pid is
    /// attached to the chat — rechecked before every send. ANY
    /// descendant holder proves it: `/proc` enumeration order is not
    /// pane-first, so a foreign holder listed earlier must not
    /// reject a valid child.
    fn verify_ownership(&self, native: &str, pane_pid: u32) -> Result<()> {
        let holders: Vec<u32> = self
            .attachments()
            .into_iter()
            .filter(|(_, chat)| chat.as_str() == native)
            .map(|(pid, _)| pid)
            .collect();
        if holders.iter().any(|pid| descends_from(*pid, pane_pid)) {
            return Ok(());
        }
        match holders.first() {
            Some(pid) => Err(Error::provider(format!(
                "Cursor chat '{native}' is owned by pid {pid} outside our pane"
            ))),
            None => Err(Error::provider(format!(
                "pane owns no live Cursor chat for '{native}'"
            ))),
        }
    }

    fn open_deadline(&self) -> Duration {
        OPEN_DEADLINE
    }

    /// Cursor's idle placeholder is verified against live captures
    /// (CAD-56 fixtures, and the padded busy frame in CAD-612).
    /// `probe.idle` is the readiness claim — the same model as Devin
    /// (CAD-520). A busy frame stays not-idle: the interrupt hint and
    /// the spinner still refuse the gate.
    fn probe_is_ready_claim(&self) -> bool {
        true
    }

    fn analyze(&self, screen: &str, cursor: Option<(u32, u32)>) -> Probe {
        analyze_cursor(screen, cursor)
    }

    fn respond_rejection(&self) -> &'static str {
        "pty endpoints have no approval channel — answer Cursor \
         permission prompts in the terminal itself"
    }

    /// The permission select's options carry their own hotkeys —
    /// `(y)`, `(tab)`, `(shift+tab)`, `(esc or n)`. A choice is the
    /// option's 1-based index; its trailing key hint is sent
    /// verbatim, or arrows + Enter when no hint prints.
    fn approval_answer(&self, screen: &str, choice: &str) -> Result<Vec<String>> {
        let n: u32 = choice.parse().map_err(|_| {
            Error::rejected(format!(
                "'{choice}' is not a menu index — Cursor menus take the \
                 option's position in printed order"
            ))
        })?;
        if n == 0 {
            return Err(Error::rejected("menu indices start at 1"));
        }
        let lines: Vec<&str> = screen.trim_end().lines().collect();
        let menu_top = lines.len().saturating_sub(MENU_LINES);
        // The option block is the whole contiguous list in printed
        // order — the `→` row may sit below earlier options, so a
        // choice indexes the block, never the suffix from `→`. When
        // the block cannot be found the answer refuses rather than
        // walking blind.
        let (sel, start, end) = menu_block(&lines[menu_top..], prompt_row(&lines[menu_top..]))
            .ok_or_else(|| {
                Error::rejected(
                    "cannot locate the option rows on this menu — answer \
                     it in the pane",
                )
            })?;
        let options = &lines[menu_top + start..=menu_top + end];
        // A `↓ more below`/`↑ more above` marker beside the block
        // means the visible list is a window onto a longer one —
        // `answer 1` would hit the first *visible* option, not the
        // first printed one. Refuse rather than pick the wrong row.
        let scroll_marker = |l: &str| {
            let t = l.trim_start();
            t.contains("more below") || t.contains("more above")
        };
        let truncated = (start > 0 && scroll_marker(lines[menu_top + start - 1]))
            || lines
                .get(menu_top + end + 1)
                .is_some_and(|l| scroll_marker(l));
        if truncated {
            return Err(Error::rejected(
                "the option list is scrolled — the visible rows are not \
                 the whole menu; answer it in the pane",
            ));
        }
        // A lone `→` row is not a one-option menu — a stray marker
        // row with chrome beneath it can parse as a block; a real
        // menu always lists at least one sibling option.
        if options.len() < 2 {
            return Err(Error::rejected(
                "cannot locate the option rows on this menu — answer \
                 it in the pane",
            ));
        }
        if n as usize > options.len() {
            return Err(Error::rejected(format!(
                "no option {n} on this menu — it lists {}",
                options.len()
            )));
        }
        if let Some(key) = hotkey(options[(n - 1) as usize]) {
            return Ok(vec![key]);
        }
        // No hotkey on the chosen row — navigate from the highlighted
        // option, which is not necessarily the first.
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

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_cursor, argv_chat, store_db_chat, CursorProfile};
    use crate::adapter::pty::profile::TuiProfile;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    /// Real captures from live cursor-agent panes (CAD-56, versions
    /// 2026.09.15/2026.09.18), committed under tests/fixtures/cursor-tui/.
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/cursor-tui/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn idle_prompt_is_pasteable() {
        let p = analyze_cursor(&fixture("idle.txt"), None);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert_eq!(p.reason, "idle");
        assert!(p.prompt_visible && !p.input_nonempty);
        assert!(!p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn follow_up_watermark_is_still_empty_input() {
        // After a turn the input placeholder flips to "Add a
        // follow-up" — a watermark, never a staged draft.
        let p = analyze_cursor(&fixture("idle-after-turn.txt"), None);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(!p.input_nonempty && !p.busy_marker);
    }

    #[test]
    fn busy_pane_blocks_on_interrupt_hint_and_spinner() {
        let p = analyze_cursor(&fixture("busy.txt"), None);
        assert!(!p.idle && p.busy_marker);
        assert_eq!(p.reason, "tui is busy (interrupt hint on the input line)");
        // The placeholder watermark is still the input text — no draft.
        assert!(!p.input_nonempty);
    }

    #[test]
    fn padded_interrupt_hint_is_empty_input_not_a_draft() {
        // Live Cursor captures (2026-09-26, CAD-612) pad the input row
        // out to the pane width *after* the right-aligned `ctrl+c to
        // stop`. The hint is then not a suffix, so reading the row as
        // a staged draft fences a turn the pane already took.
        let screen = std::fs::read_to_string(format!(
            "{}/tests/common/cursor-tui/busy-padded-hint.txt",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let line = screen
            .lines()
            .find(|l| l.contains("ctrl+c to stop"))
            .expect("recorded input row");
        assert!(
            line.ends_with(' ') && line.contains("Add a follow-up"),
            "the recording must keep the pane-width padding after the hint: {line:?}"
        );
        assert!(
            screen.contains("1 task"),
            "the live busy chrome includes the task count"
        );
        let p = analyze_cursor(&screen, None);
        assert!(!p.idle && p.busy_marker, "{}", p.reason);
        assert_eq!(p.reason, "tui is busy (interrupt hint on the input line)");
        assert!(
            !p.input_nonempty,
            "padded hint must read as a drained input, not a draft: {p:?}"
        );
    }

    #[test]
    fn staged_draft_while_busy_reports_via_spinner() {
        // A draft staged mid-turn: no interrupt hint on this frame —
        // the status row directly above the input carries the busy
        // evidence, and the draft itself is separately not-idle.
        let p = analyze_cursor(&fixture("busy-staged.txt"), None);
        assert!(!p.idle && p.busy_marker);
        assert_eq!(p.reason, "tui is busy (status row above the input line)");
        assert!(p.input_nonempty);
    }

    #[test]
    fn approval_menu_wins_over_prompt_shape() {
        // The menu's first option also leads with `→` — menu detection
        // must outrank prompt parsing or it reads as a draft.
        let p = analyze_cursor(&fixture("approval.txt"), None);
        assert!(!p.idle && p.approval_menu);
        // The reason names what the menu asks — the menu block's
        // furthest row above the option list.
        assert_eq!(p.reason, "$ whoami in .");
    }

    #[test]
    fn typed_draft_is_not_idle() {
        let p = analyze_cursor(&fixture("draft.txt"), None);
        assert!(!p.idle && p.input_nonempty);
        assert_eq!(p.reason, "unsubmitted text in the input line");
    }

    #[test]
    fn markers_absent_and_prompt_in_region_is_idle() {
        // A bare frame with the input row in the status region and a
        // status bar below it: no busy/approval evidence anywhere.
        let p = analyze_cursor(
            "  Cursor Agent\n  → Plan, search, build anything\n  Cursor Grok 4.6 High\n  /tmp/x · main\n",
            None,
        );
        assert!(p.idle && !p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn bare_prompt_as_last_row_is_not_the_input() {
        // The input row is never the frame's last row — the model/cwd
        // bar always sits below it. A bare `→` ending the frame (the
        // input row scrolled out, or transcript text) must not read
        // as a prompt: missing it fails closed, never idle.
        let p = analyze_cursor("work done\n→ ", None);
        assert!(!p.idle && !p.prompt_visible);
        assert_eq!(p.reason, "no prompt line visible");
    }

    #[test]
    fn prompt_glyph_above_status_region_is_transcript_text() {
        // A `→` row more than STATUS_LINES above the last row is
        // transcript text, not the input — e.g. a pasted glyph or a
        // menu cursor that scrolled up.
        let mut screen = String::from("work\n→ transcript arrow\n");
        for i in 0..20 {
            screen.push_str(&format!("transcript row {i}\n"));
        }
        let p = analyze_cursor(&screen, None);
        assert!(!p.idle && !p.prompt_visible);
        assert_eq!(p.reason, "no prompt line visible");
    }

    #[test]
    fn spinner_words_without_a_glyph_still_block() {
        // The status row above the input carries a spinner word + its
        // token counter even when the glyph column is unreadable.
        let p = analyze_cursor(
            "transcript\n  Thinking  12 tokens\n  → Add a follow-up\n  Cursor Grok 4.6 High\n",
            None,
        );
        assert!(!p.idle && p.busy_marker);
        assert_eq!(p.reason, "tui is busy (status row above the input line)");
    }

    #[test]
    fn queued_follow_up_box_is_busy_evidence() {
        // The `┌─ follow-ups ─┐` box interior can become the row above
        // the input — its title/footer are staged-queue evidence.
        let p = analyze_cursor(
            "work\n │ enter steer · ↑ select/edit · esc cancel │\n  → Add a follow-up\n  /mock · main\n",
            None,
        );
        assert!(!p.idle && p.busy_marker);
    }

    #[test]
    fn tip_rows_between_spinner_and_input_are_neutral() {
        // A `Tip:` row sits between the busy status row and the input
        // on real frames — skipping it must still find the spinner;
        // without one the tip alone is documentation, never busy.
        let busy = analyze_cursor(
            " ⠠⠛ Running  43 tokens\n    Tip: Try Cursor Grok 4.6 via /model\n  → Add a follow-up\n  /mock · main\n",
            None,
        );
        assert!(!busy.idle && busy.busy_marker, "{}", busy.reason);
        let idle = analyze_cursor(
            "  Tip: Use /skills to give Cursor specialized knowledge\n  → Plan, search, build anything\n  /mock · main\n",
            None,
        );
        assert!(idle.idle && !idle.busy_marker, "{}", idle.reason);
    }

    #[test]
    fn menu_options_are_not_prompt_drafts() {
        // A menu's selected option leads with `→` like the input —
        // a cluster of menu chrome in the region reads as a menu,
        // not as "unsubmitted text".
        let p = analyze_cursor(
            "  /skills\n   → + Create new skill\n   ↓ more below\n   Esc to close\n",
            None,
        );
        assert!(!p.idle && p.approval_menu);
        // The typed `/skills` line sits directly above the option
        // block — it's the menu's subject.
        assert_eq!(p.reason, "/skills");
    }

    #[test]
    fn lone_navigation_hint_is_not_a_menu() {
        // Transcript text can quote a navigation hint — one hint
        // alone never opens a menu. The `→` row still isn't a
        // prompt: ending the frame means the input scrolled out, so
        // the frame fails closed rather than idle.
        let p = analyze_cursor("work\nasked how to navigate menus\n→ ", None);
        assert!(!p.idle && !p.approval_menu);
        // Even with a real prompt, a lone hint quoted in the
        // transcript region is not approval evidence.
        let p = analyze_cursor(
            "docs say ↑↓ to navigate\n  → Plan, search, build anything\n  Grok 4.6\n",
            None,
        );
        assert!(p.idle && !p.approval_menu, "{}", p.reason);
    }

    #[test]
    fn approval_anchor_wins_over_transcript() {
        // `Not in allowlist` beside a real option row is a pending
        // approval — anchor plus menu structure decides.
        let p = analyze_cursor(
            "$ cadence self\nNot in allowlist: cadence\n  → Run (once) (y)\n    Skip & tell the agent what to do instead (esc or n)\n",
            None,
        );
        assert!(!p.idle && p.approval_menu);
    }

    /// CAD-152: no Cursor input-box shape is proven, so recover-submit
    /// cannot read a Cursor draft — the profile keeps the fail-closed
    /// default and names the attach route.
    #[test]
    fn draft_rows_refuse_for_cursor() {
        let err = screen_profile()
            .draft_rows("→ Kickoff AOS-11: read the brief")
            .unwrap_err();
        assert!(
            err.contains("Cursor drafts cannot be read reliably") && err.contains("agent attach"),
            "{err}"
        );
    }

    /// A profile for `approval_answer` tests — the method only reads
    /// the screen, so the launch fields are dummies.
    fn screen_profile() -> CursorProfile {
        CursorProfile {
            chats_dir: PathBuf::from("/tmp"),
            command: "cursor-agent".to_string(),
            model: None,
            permission_mode: None,
        }
    }

    /// The permission select's option block (from `approval.txt`) —
    /// every option carries its hotkey.
    fn permission_menu() -> &'static str {
        " $  whoami in .\n Run this command?\n Not in allowlist: whoami\n  → Run (once) (y)\n    Add Shell(whoami) to allowlist? (tab)\n    Run Everything (shift+tab)\n    Skip & tell the agent what to do instead (esc or n)\n"
    }

    #[test]
    fn answer_sends_the_chosen_options_hotkey() {
        let prof = screen_profile();
        assert_eq!(
            prof.approval_answer(permission_menu(), "1").unwrap(),
            vec!["y"]
        );
        assert_eq!(
            prof.approval_answer(permission_menu(), "3").unwrap(),
            vec!["BTab"]
        );
        // `(esc or n)` takes the last alternative.
        assert_eq!(
            prof.approval_answer(permission_menu(), "4").unwrap(),
            vec!["n"]
        );
    }

    #[test]
    fn answer_counts_options_above_the_highlight() {
        // The `→` highlight sits on the SECOND printed option — option
        // 1 is the row above it, so its hotkey (`y`), never the
        // highlighted row's (`tab`).
        let screen = " Run this command?\n Not in allowlist: whoami\n    Run (once) (y)\n  → Add Shell(whoami) to allowlist? (tab)\n    Run Everything (shift+tab)\n";
        let prof = screen_profile();
        assert_eq!(prof.approval_answer(screen, "1").unwrap(), vec!["y"]);
        assert_eq!(prof.approval_answer(screen, "2").unwrap(), vec!["Tab"]);
        assert_eq!(prof.approval_answer(screen, "3").unwrap(), vec!["BTab"]);
    }

    #[test]
    fn answer_index_is_bounded_by_the_visible_block() {
        let prof = screen_profile();
        for choice in ["5", "4000000000", "0"] {
            assert!(
                prof.approval_answer(permission_menu(), choice).is_err(),
                "{choice} must refuse"
            );
        }
        // Chrome with no `→` option block refuses outright — never a
        // blind arrow walk.
        let anchor_only = " Run this command?\n Not in allowlist: whoami\n";
        assert!(prof.approval_answer(anchor_only, "1").is_err());
        assert!(prof.approval_answer(anchor_only, "4000000000").is_err());
    }

    #[test]
    fn hotkey_hint_outside_the_allowlist_falls_back() {
        // `(C-c)` is not a menu key — the allowlist sends arrows
        // instead of passing the token to tmux.
        let screen = " Run this command?\n  → Run (once) (C-c)\n    Cancel (esc or n)\n";
        let prof = screen_profile();
        assert_eq!(prof.approval_answer(screen, "1").unwrap(), vec!["Enter"]);
        assert_eq!(prof.approval_answer(screen, "2").unwrap(), vec!["n"]);
    }

    #[test]
    fn quoted_anchor_text_is_not_a_menu() {
        // The anchor strings quoted mid-line in a transcript stay
        // text — detection matches on the trimmed row's leading text
        // (or requires the option block for `Waiting for approval`).
        let busy = fixture("busy.txt");
        for quoted in [
            "earlier the agent asked \"Run this command?\" — transcript",
            "the log shows \"Not in allowlist\" in passing",
            "it printed \"$ x Waiting for approval...\" mid-line once",
        ] {
            let p = analyze_cursor(&format!("{busy}\n{quoted}"), None);
            assert!(!p.approval_menu, "{quoted}: {:?}", p);
        }
    }

    #[test]
    fn quoted_hint_text_is_not_a_menu() {
        // Footer fragments mid-line are not menu rows — a menu needs
        // the anchored legend or the `→` option block beside them.
        let busy = fixture("busy.txt");
        for quoted in [
            "docs say \"Esc to close\" dismisses pickers",
            "scroll down for more below the fold",
        ] {
            let p = analyze_cursor(&format!("{busy}\n{quoted}"), None);
            assert!(!p.approval_menu, "{quoted}: {:?}", p);
        }
    }

    #[test]
    fn indented_anchor_without_structure_is_not_a_menu() {
        // A transcript row that LEADS with the anchor once indented
        // is the round-3 false positive: `trim_start` makes it look
        // row-anchored, and the anchor alone used to flip the pane
        // to `approval_menu` and livelock every send. With no option
        // block and no legend beside it, the row is inert text.
        let busy = fixture("busy.txt");
        for line in [
            "    Run this command?",
            "      Not in allowlist: rg",
            "  → waiting earlier: Run this command?", // mid-row anchor
        ] {
            let p = analyze_cursor(&format!("{busy}\n{line}"), None);
            assert!(!p.approval_menu, "{line}: {:?}", p);
        }
    }

    #[test]
    fn parenthesized_transcript_line_is_not_an_option() {
        // `foo(bar)` is not a hotkey — a `→` transcript row plus that
        // line used to parse as a two-option block, letting `answer`
        // send Enter into a real input box. `opt_row` now requires
        // an allowlisted hotkey on non-selected rows, so no block
        // forms and neither detect nor answer engages.
        let screen = "echo ran ok\n  → ls -la\n foo(bar)\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        let prof = screen_profile();
        assert!(prof.approval_answer(screen, "1").is_err());
        // And the input-box case outright: `→` input prompt over a
        // parenthesized transcript row.
        let prompt = " foo(bar)\n  → \n  Grok 4.6\n";
        assert!(!analyze_cursor(prompt, None).approval_menu);
        assert!(prof.approval_answer(prompt, "1").is_err());
    }

    #[test]
    fn last_row_highlight_still_detects_and_answers() {
        // A highlighted option can be the frame's last row — nothing
        // renders below it. The block must still qualify via the
        // option row above it.
        let screen = " Run this command?\n Not in allowlist: whoami\n    Run (once) (y)\n  → Skip & tell the agent what to do instead (esc or n)\n";
        let p = analyze_cursor(screen, None);
        assert!(p.approval_menu, "{p:?}");
        let prof = screen_profile();
        assert_eq!(prof.approval_answer(screen, "1").unwrap(), vec!["y"]);
        assert_eq!(prof.approval_answer(screen, "2").unwrap(), vec!["n"]);
    }

    #[test]
    fn scrolled_option_block_refuses_to_answer() {
        // `↓ more below` under the block means option 1 is off-screen
        // — answering `1` would hit the first *visible* row instead.
        let screen = " Run this command?\n    Run (once) (y)\n    Add Shell(whoami) to allowlist? (tab)\n  → Skip & tell the agent what to do instead (esc or n)\n   ↓ more below\n";
        let prof = screen_profile();
        let err = prof.approval_answer(screen, "1").unwrap_err().to_string();
        assert!(err.contains("scrolled"), "{err}");
        // Detection still sees the menu — the refusal is only about
        // which row a number picks.
        assert!(analyze_cursor(screen, None).approval_menu);
        // A marker ABOVE the block is the same truncation.
        let screen = " Run this command?\n   ↑ more above\n  → Add Shell(whoami) to allowlist? (tab)\n    Skip & tell the agent what to do instead (esc or n)\n";
        assert!(prof.approval_answer(screen, "1").is_err());
    }

    #[test]
    fn transcript_echoes_without_keys_are_not_a_menu() {
        // Two adjacent `→` transcript echoes carry no `(key)` hint —
        // every real option row advertises one, so a block of echoes
        // must not qualify even with an anchor on screen.
        let screen =
            " Run this command?\n  → earlier submitted prompt\n  → another submitted prompt\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        let prof = screen_profile();
        assert!(prof.approval_answer(screen, "1").is_err());
    }

    #[test]
    fn rust_style_parens_are_not_option_keys() {
        // Round-4 review: `hotkey` used to accept ANY single ascii
        // alphanumeric inside a trailing paren — `Ok(v)`, `Some(x)`,
        // `f(1)`, `Err(e)` all parsed as keyed option rows, so Rust
        // output under an anchor could fake a menu block. The key must
        // end the row AND come from the named/observed allowlist.
        let prof = screen_profile();
        for row in [" foo(b)", " Ok(v)", " Some(x)", " f(1)", " Err(e)"] {
            let screen = format!(" Run this command?\n{row}\n  → ls -la\n");
            assert!(!analyze_cursor(&screen, None).approval_menu, "{row}");
            assert!(prof.approval_answer(&screen, "1").is_err(), "{row}");
        }
        // The real keys still resolve: `(y)`, `(tab)`, `(esc or n)`.
        assert_eq!(
            prof.approval_answer(permission_menu(), "1").unwrap(),
            vec!["y"]
        );
    }

    #[test]
    fn input_row_is_never_a_menu_highlight() {
        // A keyed transcript row directly above the live input line
        // used to make the input itself the block's selected option —
        // `answer` then sent Enter into a staged draft. The prompt row
        // is excluded from menu parsing outright.
        let screen = " Run this command?\n    Add Shell(whoami) to allowlist? (tab)\n  → my staged draft\n  Grok 4.6 · ~/repo\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        // The draft still reads as a draft — menu refusal must not
        // blind the draft signal.
        assert!(
            p.input_nonempty && p.reason == "unsubmitted text in the input line",
            "{p:?}"
        );
        let prof = screen_profile();
        assert!(prof.approval_answer(screen, "1").is_err());
    }

    #[test]
    fn function_call_parens_are_not_key_hints() {
        // Round-5 review: the allowlist alone left `let v = take(n)`
        // keyed — `(n)` at end-of-row satisfied it. The hint paren
        // must be a separate trailing token, whitespace-separated from
        // the label — function-call parens never qualify.
        for row in [
            "    let v = take(n)",
            "    return f(y)",
            "    exit(status)",
            "    Option::Some(esc)",
        ] {
            let screen = format!(" Run this command?\n{row}\n{row}\n  → Run (once) (y)\n");
            assert!(
                !analyze_cursor(&screen, None).approval_menu,
                "{row} must not be a keyed row"
            );
        }
        // Real whitespace-separated hints still parse.
        for row in [
            "    Run (once) (y)",
            "    Skip (esc or n)",
            "    Add to allowlist? (tab)",
        ] {
            let screen = format!(" Run this command?\n  → Run (once) (y)\n{row}\n");
            let p = analyze_cursor(&screen, None);
            assert!(p.approval_menu, "{row}");
        }
    }

    #[test]
    fn unidentified_input_row_cannot_become_sel() {
        // Round-5 should-fix: when the input `→` row fails its frame
        // test — the model bar is cut off the capture — `prompt_row`
        // is None and the row used to become the block's selected
        // option, letting `answer` send a bare Enter into a staged
        // draft. The keyed-above path now needs the `→` row's own
        // `(key)`, which a draft never carries.
        let screen =
            " Run this command?\n    Add Shell(whoami) to allowlist? (tab)\n  → deploy to prod\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu, "{p:?}");
        let prof = screen_profile();
        assert!(prof.approval_answer(screen, "1").is_err());
        // The same draft mid-frame (model bar visible) stays a draft.
        let screen = " Run this command?\n    Add Shell(whoami) to allowlist? (tab)\n  → deploy to prod\n  Grok 4.6 · ~/repo\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu && p.input_nonempty, "{p:?}");
        // And a draft ENDING in a key-looking suffix is still a draft
        // — the `(y)` is typed text, never the row's own hotkey.
        let screen = " Run this command?\n    Add Shell(whoami) to allowlist? (tab)\n  → deploy to prod (y)\n  Grok 4.6 · ~/repo\n";
        let p = analyze_cursor(screen, None);
        assert!(!p.approval_menu && p.input_nonempty, "{p:?}");
        assert!(prof.approval_answer(screen, "y").is_err());
    }

    fn profile(dir: &std::path::Path, params: serde_json::Value) -> CursorProfile {
        CursorProfile {
            // A real temp dir — `launch_command` merges the allowlist
            // into `<chats>/../cli-config.json`, so a bogus path would
            // write a config at the filesystem root in tests.
            chats_dir: dir.join("chats"),
            command: "cursor-agent".to_string(),
            model: params
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            permission_mode: params
                .get("permission_mode")
                .and_then(|v| v.as_str())
                .map(str::to_string),
        }
    }

    #[test]
    fn launch_argv_resume_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        let p = profile(
            dir.path(),
            json!({"model": "grok-4", "permission_mode": "force"}),
        );
        assert_eq!(
            p.launch_command(Some("chat-1")).unwrap(),
            format!(
                "{} --trust --model 'grok-4' --force --resume 'chat-1'",
                p.command
            )
        );
        let p = profile(dir.path(), json!({"permission_mode": "auto-review"}));
        assert_eq!(
            p.launch_command(Some("chat-2")).unwrap(),
            format!("{} --trust --auto-review --resume 'chat-2'", p.command)
        );
        // An unset mode keeps cursor's own default — no flag emitted.
        let p = profile(dir.path(), json!({}));
        assert_eq!(
            p.launch_command(Some("chat-3")).unwrap(),
            format!("{} --trust --resume 'chat-3'", p.command)
        );
        // Values are shell-quoted even if hand-edited past validation.
        let p = profile(dir.path(), json!({"permission_mode": "bogus; rm -rf /"}));
        assert_eq!(
            p.launch_command(Some("c'hat")).unwrap(),
            format!("{} --trust --resume 'c'\\''hat'", p.command)
        );
    }

    #[test]
    fn ownership_proofs() {
        // store.db fd proof: a process holding
        // <chats>/<hash>/<chat>/store.db owns that chat.
        let dir = tempfile::tempdir().unwrap();
        let chats = dir.path().join("chats");
        let db = chats.join("abc123").join("chat-9").join("store.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&db).unwrap();
        let pid = std::process::id();
        assert_eq!(
            store_db_chat(&chats, pid).as_deref(),
            Some("chat-9"),
            "our own fd on store.db must prove chat-9"
        );
        drop(file);
        // argv proof: only a cursor-agent argv[0] counts — our own
        // test process's cmdline names no chat.
        assert_eq!(argv_chat(pid), None);
        // A trailing slash in the chats dir must not break the
        // fd-link prefix match.
        let file = std::fs::File::create(&db).unwrap();
        assert_eq!(
            store_db_chat(&chats.join(""), pid).as_deref(),
            Some("chat-9"),
        );
        drop(file);
    }

    #[test]
    fn argv_proof_fabricated() {
        // A process whose argv[0] names cursor-agent and carries
        // `--resume <chat>` proves that chat — fabricated with
        // `exec -a` so the test needs no real binary.
        let mut holder = std::process::Command::new("bash")
            .args([
                "-c",
                "exec -a cursor-agent python3 -c 'import time; time.sleep(30)' --resume chat-fab-9",
            ])
            .spawn()
            .unwrap();
        let mut found = None;
        for _ in 0..100 {
            if let Some(chat) = argv_chat(holder.id()) {
                found = Some(chat);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(found.as_deref(), Some("chat-fab-9"));
        // An argv[0] that does not name cursor-agent proves nothing
        // even with `--resume` on its cmdline.
        let mut other = std::process::Command::new("bash")
            .args([
                "-c",
                "exec -a bash python3 -c 'import time; time.sleep(30)' --resume chat-not-mine",
            ])
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(argv_chat(other.id()), None);
        let _ = holder.kill();
        let _ = other.kill();
        let _ = holder.wait();
        let _ = other.wait();
    }

    /// A profile rooted in a temp dir: `cli-config.json` lands next
    /// to `chats/` inside it.
    fn profile_in(dir: &std::path::Path) -> CursorProfile {
        CursorProfile {
            chats_dir: dir.join("chats"),
            command: "cursor-agent".to_string(),
            model: None,
            permission_mode: None,
        }
    }

    fn allow_entries(dir: &std::path::Path) -> Vec<String> {
        let text = std::fs::read_to_string(dir.join("cli-config.json")).unwrap();
        serde_json::from_str::<Value>(&text).unwrap()["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.as_str().unwrap().to_string())
            .collect()
    }

    /// CAD-310: the sandbox gate guards the write, not the launch. An
    /// entry already present never asks; a missing one is refused
    /// before a byte is written or backed up.
    #[test]
    fn allowlist_gate_runs_only_when_the_merge_would_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = profile_in(dir.path());
        let config = dir.path().join("cli-config.json");
        let refuse = || -> crate::error::Result<()> {
            Err(crate::error::Error::rejected("gate: sandbox refused"))
        };
        std::fs::write(
            &config,
            r#"{"version":1,"permissions":{"allow":["Shell(cadence)"],"deny":[]}}"#,
        )
        .unwrap();
        let before = std::fs::read(&config).unwrap();
        p.merge_cadence_allowlist(&refuse).unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), before);
        std::fs::write(
            &config,
            r#"{"version":1,"permissions":{"allow":[],"deny":[]}}"#,
        )
        .unwrap();
        let before = std::fs::read(&config).unwrap();
        let err = p.merge_cadence_allowlist(&refuse).unwrap_err();
        assert!(err.to_string().contains("gate: sandbox refused"), "{err}");
        assert_eq!(std::fs::read(&config).unwrap(), before);
        assert!(!dir.path().join("cli-config.json.bak").exists());
    }

    #[test]
    fn allowlist_merge_creates_config_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        assert_eq!(allow_entries(dir.path()), ["Shell(cadence)"]);
        assert!(!dir.path().join("cli-config.json.bak").exists());
        // Idempotent: a second run leaves the file untouched.
        let before = std::fs::read_to_string(dir.path().join("cli-config.json")).unwrap();
        p.ensure_cadence_allowlist().unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cli-config.json")).unwrap(),
            before
        );
    }

    #[test]
    fn allowlist_merge_preserves_document() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cli-config.json");
        // `permissions` without `allow` — the array is created.
        std::fs::write(
            &config,
            "{\n  \"permissions\": {\"deny\": []},\n  \"display\": {\"mode\": \"zen\"}\n}\n",
        )
        .unwrap();
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["permissions"]["allow"].as_array().unwrap(),
            &vec![Value::String("Shell(cadence)".to_string())]
        );
        assert_eq!(doc["display"]["mode"].as_str().unwrap(), "zen");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cli-config.json.bak")).unwrap(),
            "{\n  \"permissions\": {\"deny\": []},\n  \"display\": {\"mode\": \"zen\"}\n}\n"
        );
    }

    #[test]
    fn allowlist_merge_existing_entry_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cli-config.json");
        std::fs::write(
            &config,
            "{\"permissions\": {\"allow\": [\"Shell(ls)\", \"Shell(cadence)\"]}}",
        )
        .unwrap();
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        // Entry present → no write, no .bak, bytes unchanged.
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "{\"permissions\": {\"allow\": [\"Shell(ls)\", \"Shell(cadence)\"]}}"
        );
        assert!(!dir.path().join("cli-config.json.bak").exists());
    }

    #[test]
    fn allowlist_merge_refuses_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cli-config.json");
        std::fs::write(&config, "{ not json").unwrap();
        let p = profile_in(dir.path());
        let err = p.ensure_cadence_allowlist().unwrap_err().to_string();
        assert!(err.contains("not valid JSON"), "{err}");
        assert!(err.contains("refusing to launch"), "{err}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{ not json");
        // A well-formed file with a non-array allow is malformed too.
        std::fs::write(&config, "{\"permissions\": {\"allow\": \"cadence\"}}").unwrap();
        let err = p.ensure_cadence_allowlist().unwrap_err().to_string();
        assert!(err.contains("malformed"), "{err}");
        assert!(err.contains("`permissions.allow` is not an array"), "{err}");
    }

    #[test]
    fn allowlist_merge_survives_a_crashed_write() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cli-config.json");
        std::fs::write(&config, "{\"permissions\": {\"allow\": [\"Shell(ls)\"]}}").unwrap();
        // A crash mid-write leaves a uniquely-named temp sibling behind
        // and the original bytes intact — never a torn config.
        let tmp = PathBuf::from(format!(
            "{}.cadence-tmp.{}.{}",
            config.display(),
            std::process::id(),
            "deadbeefdeadbeefdeadbeefdeadbeef"
        ));
        std::fs::write(&tmp, "{\"partial json that never").unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "{\"permissions\": {\"allow\": [\"Shell(ls)\"]}}"
        );
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        // The stale temp is abandoned, not consumed — the next merge
        // uses a fresh unique name; the merged file is whole and ordered.
        assert_eq!(allow_entries(dir.path()), ["Shell(ls)", "Shell(cadence)"]);
        // The `.bak` kept the original bytes — and its mode.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cli-config.json.bak")).unwrap(),
            "{\"permissions\": {\"allow\": [\"Shell(ls)\"]}}"
        );
    }

    #[test]
    fn allowlist_merge_keeps_the_source_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cli-config.json");
        std::fs::write(&config, "{\"permissions\": {\"allow\": []}}").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        // Config and backup are created with the source's mode from
        // the start — no world-readable window on the auth file.
        assert_eq!(mode(&config), 0o640);
        assert_eq!(mode(&dir.path().join("cli-config.json.bak")), 0o640);
    }

    #[test]
    fn allowlist_merge_writes_through_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-config.json");
        std::fs::write(&real, "{\"permissions\": {\"allow\": [\"Shell(ls)\"]}}").unwrap();
        let link = dir.path().join("cli-config.json");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let p = profile_in(dir.path());
        p.ensure_cadence_allowlist().unwrap();
        // The link survived as a link — the merge landed on the
        // target, not on a regular file that replaced the symlink.
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(allow_entries(dir.path()), ["Shell(ls)", "Shell(cadence)"]);
        // `.bak` and the atomic temp landed beside the target.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real-config.json.bak")).unwrap(),
            "{\"permissions\": {\"allow\": [\"Shell(ls)\"]}}"
        );
    }

    #[test]
    fn unrecorded_pane_chat_is_never_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let p = profile_in(dir.path());
        // (None, Some): a pane holding a chat nobody recorded — e.g.
        // after the stored id was cleared — must not be adopted.
        // Refusing keeps the boundary fail-closed; the next open
        // mints a fresh chat.
        let err = p
            .resolve_session(None, Some("foreign-chat".to_string()))
            .unwrap_err();
        assert!(err.to_string().contains("refusing to adopt"), "{err}");
        // The wanted-chat cases are unchanged.
        assert_eq!(
            p.resolve_session(Some("a"), Some("a".to_string())).unwrap(),
            "a"
        );
        assert!(p.resolve_session(Some("a"), Some("b".to_string())).is_err());
    }

    /// `prepare_session` runs `sh -c "<command> create-chat"` — a
    /// fabricated command prints whatever shape the test needs. It
    /// also merges the allowlist first, so the profile needs a real
    /// tempdir (`<chats>/../cli-config.json`), not a bogus path. The
    /// dir only has to outlive the call: the merge and the mint both
    /// finish before `prepare_session` returns.
    fn mint_with(command: &str) -> crate::error::Result<Option<String>> {
        let dir = tempfile::tempdir().unwrap();
        CursorProfile {
            chats_dir: dir.path().join("chats"),
            command: command.to_string(),
            model: None,
            permission_mode: None,
        }
        .prepare_session()
    }

    #[test]
    fn mint_anchors_on_uuid_shape() {
        // Warnings around the id are fine — only the uuid token
        // counts.
        let p = mint_with("printf 'a warning line\\n%s\\n' 11111111-2222-3333-4444-555555555555");
        assert_eq!(
            p.unwrap().as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        // No id-shaped token at all fails closed.
        let err = mint_with("echo create-chat failed upstream")
            .unwrap_err()
            .to_string();
        assert!(err.contains("printed no chat id"), "{err}");
        // Two different ids is ambiguous — never pick one.
        let err = mint_with(
            "printf '%s\\n%s\\n' 11111111-2222-3333-4444-555555555555 aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ambiguous"), "{err}");
        // A failing command reports its stderr.
        let err = mint_with("false").unwrap_err().to_string();
        assert!(err.contains("create-chat failed"), "{err}");
    }
}
