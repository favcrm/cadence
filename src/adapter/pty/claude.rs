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

use crate::adapter::{Probe, ProviderEnv};
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
/// The wider window a menu may occupy: a permission prompt (~10 rows)
/// plus the input box and status bar that can stay visible below it.
/// Menu anchors only match inside it — the transcript above can
/// legitimately quote the same strings.
const MENU_LINES: usize = 24;

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
    /// An open select/permission menu — the anchors are menu-exclusive
    /// (the permission prompt's title, plus the workspace-trust dialog
    /// in both its observed 2.1.x wordings) and a single match decides
    /// alone. The hints are footer fragments — quotable in a long
    /// transcript, so they only count as a cluster alongside real menu
    /// structure (a second hint, or numbered option rows).
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
    // Menu evidence lives in the wider menu window — a permission
    // prompt plus a still-visible input box can push its rows above
    // the busy anchor. Anchors decide alone; hints need real menu
    // structure beside them (numbered options or a second hint).
    let menu_lines: Vec<&str> = content.lines().rev().take(MENU_LINES).collect();
    let anchor = menu_lines
        .iter()
        .any(|l| claude_screen::ANCHOR.iter().any(|a| l.contains(a)));
    let options = menu_lines
        .iter()
        .filter(|l| option_line(l).is_some())
        .count();
    let hints = claude_screen::HINT
        .iter()
        .filter(|h| menu_lines.iter().any(|l| l.contains(**h)))
        .count();
    let approval_menu = anchor || (options >= 2 && hints >= 1) || hints >= 2;
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
        (
            false,
            menu_subject(content).unwrap_or_else(|| "approval menu is open".to_string()),
        )
    } else if busy_marker {
        (
            false,
            "tui is busy (interrupt marker on screen)".to_string(),
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
        let numbered: Vec<u32> = region.iter().filter_map(|l| option_line(l)).collect();
        let n: u32 = choice.parse().map_err(|_| {
            Error::rejected(format!(
                "'{choice}' is not a menu index — Claude menus take the \
                 option's printed number"
            ))
        })?;
        if !numbered.is_empty() {
            if numbered.contains(&n) {
                return Ok(vec![n.to_string()]);
            }
            return Err(Error::rejected(format!(
                "no option {n} on this menu — it lists {}",
                numbered
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        // Unnumbered select: option rows sit between the first
        // `❯`-led row (skipping a boxed input line — its previous row
        // is the `─` border) and the footer (hint-bearing) row.
        let sel = region
            .iter()
            .enumerate()
            .position(|(i, l)| {
                l.trim_start().starts_with('❯')
                    && !(i > 0
                        && region[i - 1]
                            .trim()
                            .chars()
                            .all(|c| c == claude_screen::BORDER)
                        && !region[i - 1].trim().is_empty())
            })
            .ok_or_else(|| Error::rejected("no menu options on screen"))?;
        let count = region[sel..]
            .iter()
            .take_while(|l| !claude_screen::HINT.iter().any(|h| l.contains(*h)))
            .filter(|l| !l.trim().is_empty())
            .count() as u32;
        if n == 0 || n > count {
            return Err(Error::rejected(format!(
                "no option {n} on this menu — it lists {count}"
            )));
        }
        let mut keys = vec!["Down".to_string(); (n - 1) as usize];
        keys.push("Enter".to_string());
        Ok(keys)
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
