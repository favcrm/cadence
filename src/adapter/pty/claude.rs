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
//! input box shows a dim "ghost" suggestion (`❯  ls -l …`) that a
//! plain-text capture cannot tell from a staged draft. The suggestion
//! never moves the cursor — real input does — so `input_nonempty`
//! consults the pane cursor position the adapter passes to
//! [`TuiProfile::analyze`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::adapter::Probe;
use crate::error::{Error, Result};
use crate::store::Agent;

use super::profile::TuiProfile;
use super::{descends_from, resolve_on_path, shlex_quote};

/// Bounded wait for the launched Claude TUI to publish its session
/// registry entry.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// How much of the screen bottom counts as the status region: input
/// box, status bar and a menu tall enough for Claude's permission
/// select. Busy/approval markers only match inside it — the transcript
/// above can legitimately show these strings as text.
const STATUS_LINES: usize = 16;

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
    /// An open select/permission menu — the permission prompt's title
    /// and footer, plus the workspace-trust dialog (both its observed
    /// 2.1.x wording and the older prompt shape).
    pub const APPROVAL: &[&str] = &[
        "Do you want to proceed?",
        "Esc to cancel",
        "Tab to amend",
        "Quick safety check",
        "Yes, I trust this folder",
        "Do you trust the files in this folder?",
    ];
}

/// Leading characters Claude's TUI interprets before the prompt text —
/// observed live in a scratch pane (CAD-17): `!` switches to shell
/// mode, `/` opens the command menu, `@` opens the agent/file picker.
/// `#` stays a literal draft and is deliberately absent.
pub const FORBIDDEN_PREFIXES: &[char] = &['/', '!', '@'];

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
/// content.
pub fn analyze_claude(screen: &str, cursor: Option<(u32, u32)>) -> Probe {
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
    let approval_menu = claude_screen::APPROVAL.iter().any(|m| tail.contains(m));
    let busy_marker = claude_screen::BUSY.iter().any(|m| tail.contains(m));
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
    let draft = prompt
        .map(|(_, l)| {
            l.trim_start()
                .trim_start_matches(claude_screen::PROMPT)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    // Ghost suggestion: dim text in an empty box never moves the
    // cursor off the prompt start — a real draft always does. Unknown
    // cursor counts as a real draft (conservative).
    let ghost = !draft.is_empty()
        && match (cursor, prompt) {
            (Some((x, y)), Some((row, _))) => y as usize == row && x <= 2,
            _ => false,
        };
    let input_nonempty = !draft.is_empty() && !ghost;
    let (idle, reason) = if approval_menu {
        (false, "approval menu is open")
    } else if busy_marker {
        (false, "tui is busy (interrupt marker on screen)")
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
/// agent and the Stop hook need them for `cadence self`.
fn scrubbed_env_names() -> Vec<String> {
    const KEEP: &[&str] = &[
        "CLAUDE_CONFIG_DIR",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CADENCE_ALIAS",
        "CADENCE_STATE_DIR",
    ];
    std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| {
            (k.starts_with("CLAUDE_")
                || k.starts_with("CLAUDECODE")
                || k.starts_with("CODEX_")
                || k.starts_with("CADENCE_"))
                && !KEEP.contains(&k.as_str())
        })
        .collect()
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
    pub fn new(agent: &Agent) -> Result<Self> {
        let sessions_dir = std::env::var("CADENCE_CLAUDE_SESSIONS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude/sessions")
            });
        let (command, real) = match std::env::var("CADENCE_CLAUDE_TUI_COMMAND") {
            Ok(cmd) if !cmd.is_empty() => (cmd, false),
            _ => (resolve_on_path("claude").map(|p| shlex_quote(&p))?, true),
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
        let mut allowed_tools = vec!["Bash(cadence *)".to_string()];
        if let Some(list) = params.get("allowed_tools").and_then(Value::as_array) {
            allowed_tools.extend(list.iter().filter_map(Value::as_str).map(str::to_string));
        }
        Ok(Self {
            sessions_dir,
            command,
            model,
            permission_mode,
            allowed_tools,
            real,
        })
    }

    /// Every live session-registry entry — parsed `<pid>.json` files
    /// whose process is still running under the recorded start tick.
    /// A transient read miss silently drops an entry, so an all-empty
    /// result is re-scanned before it is believed: a registry nobody
    /// occupies stays empty, a raced one does not.
    fn live_entries(&self) -> Vec<SessionEntry> {
        let mut entries = Vec::new();
        for attempt in 0..super::EVIDENCE_PROBES {
            entries = std::fs::read_dir(&self.sessions_dir)
                .map(|dir| {
                    dir.flatten()
                        .filter_map(|e| self.read_entry(&e.path()))
                        .filter(SessionEntry::alive)
                        .collect()
                })
                .unwrap_or_default();
            if !entries.is_empty() || attempt + 1 == super::EVIDENCE_PROBES {
                break;
            }
            std::thread::sleep(super::EVIDENCE_SETTLE);
        }
        entries
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

    fn respond_rejection(&self) -> &'static str {
        "pty endpoints have no approval channel — answer Claude \
         permission prompts in the terminal itself"
    }

    fn forbidden_prefixes(&self) -> &'static [char] {
        FORBIDDEN_PREFIXES
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_claude, proc_start_ticks, STATUS_LINES};

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

    #[test]
    fn approval_menu_wins_over_prompt_shape() {
        // The permission select's first option leads with `❯` too —
        // menu detection must outrank prompt parsing or it reads as a
        // draft. The menu replaces the box entirely (no boxed ❯ here).
        let p = analyze_claude(&fixture("approval.txt"), None);
        assert!(!p.idle);
        assert!(p.approval_menu);
        assert_eq!(p.reason, "approval menu is open");
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
}
