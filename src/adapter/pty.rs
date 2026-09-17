//! Devin's official terminal UI, driven through an owned tmux session.
//!
//! The daemon launches `devin` inside a detached tmux session on a private
//! socket (`cadence-<state-hash>`), so every pane it may kill is one it
//! spawned. Native session ownership is *proven*, not assumed: the pane's
//! process tree must hold the flock at
//! `~/.local/share/devin/cli/session_locks/<session>.lock`. A lock held by
//! a foreign process means another TUI owns the session — open refuses
//! rather than taking it over.
//!
//! Submission is gated, never blind: every `run_turn` re-verifies the pane
//! is alive, unblocked (`pane_in_mode == 0`) and still lock-owning, and
//! consumes a readiness claim. Claims are single-use, time-boxed
//! ([`READY_TTL`]) and stack FIFO — N claims release N queued sends, each
//! attributed to its claimer for the audit record. Agents opted into
//! `params.auto_ready = "verified"` let the daemon mint the claim itself
//! after a screen probe ([`analyze_devin`]) proves the pane idle; a human
//! `agent ready` still wins whenever both exist.
//!
//! Text is delivered literally through a tmux buffer (`load-buffer` +
//! bracketed `paste-buffer -p` + `Enter`); no shell interpolation and no
//! control characters. After Enter, a differential render check must see
//! this paste's text newly on the visible screen *and* the input line
//! empty again within [`RENDER_DEADLINE`] — a busy TUI drops a bracketed
//! paste silently, so bytes-sent is not delivery evidence. A miss inside
//! the bound is `NotRendered` — evidence, not proof, which is why the
//! daemon parks exhausted informational deliveries and fences task
//! messages as `unknown` rather than failing them.
//!
//! Terminal echo proves *rendering*, never model receipt. A pasted
//! message stays `running` under its `pty-<generation>-<uuid>` token
//! until an explicit `message ack` / `message result` report completes
//! it; tokens from a previous endpoint generation are rejected. If the
//! endpoint dies after a possible paste the outcome is `unknown` and the
//! attempt is never replayed.
//!
//! Approval prompts are not brokered: `agent respond` is rejected for
//! this endpoint — answer them in the terminal itself.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::store::Agent;

use super::{AdapterHooks, Identity, Probe, ProviderAdapter, TurnResult};

/// How long an operator readiness claim stays valid for one send.
const READY_TTL: Duration = Duration::from_secs(60);
/// Bounded wait for the launched TUI to take a native session lock.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// Pause between bracketed paste and Enter so the TUI consumes it.
const PASTE_SETTLE: Duration = Duration::from_millis(300);
/// Bounded post-paste wait for the body to render in the transcript.
/// A miss inside the bound is *evidence* of a dropped paste, not proof
/// — a saturated host renders late — which is why a task message lands
/// `unknown` (uncertainty discipline) rather than `failed`.
const RENDER_DEADLINE: Duration = Duration::from_secs(4);
/// How much of the screen bottom counts as the status region: input
/// line, divider, status bar and a menu tall enough for Devin's
/// approval select. Busy/approval markers only match inside it — the
/// transcript above can legitimately show these strings as text.
const STATUS_LINES: usize = 14;
/// Slice of the pasted body used for the differential render check.
/// The tail is what stays visible: a long input scrolls horizontally
/// to the cursor, and a wrapped transcript ends with it.
const PROBE_SLICE: usize = 64;
/// Stacked operator claims retained for the queue (oldest dropped past
/// this); each claim releases exactly one gated message.
const CLAIM_CAPACITY: usize = 16;
const BUFFER: &str = "cadence-msg";

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
    /// On-screen markers while a turn is running.
    pub const BUSY: &[&str] = &[
        "(esc again to interrupt)",
        "(esc twice to interrupt)",
        "Cancel agent (esc twice)",
        "Guide Devin while it works",
        "Press Ctrl+O to view the full thinking trace",
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

/// Reduce a captured Devin screen to gate facts. The last `❭` line is
/// the input line; text after it that is not the placeholder is a
/// staged draft. Menus and busy markers win over prompt parsing — a
/// `❭` leads the first approval option too — and both are only read
/// in the bottom status region: the transcript above can legitimately
/// print these same strings (source text, docs) without the pane being
/// busy at all.
pub fn analyze_devin(screen: &str) -> Probe {
    let tail: String = screen
        .lines()
        .rev()
        .take(STATUS_LINES)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let approval_menu = devin_screen::APPROVAL.iter().any(|m| tail.contains(m));
    let busy_marker = devin_screen::BUSY
        .iter()
        .chain(devin_screen::QUEUED.iter())
        .any(|m| tail.contains(m));
    let prompt_line = screen
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with(devin_screen::PROMPT));
    let prompt_visible = prompt_line.is_some();
    let draft = prompt_line
        .map(|l| {
            l.trim_start()
                .trim_start_matches(devin_screen::PROMPT)
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let input_nonempty = !draft.is_empty()
        && !draft.starts_with(devin_screen::PLACEHOLDER)
        && !draft.starts_with(devin_screen::BUSY_PLACEHOLDER);
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

/// One operator readiness claim: single-use, time-boxed, attributed to
/// the claimer when known (the pane's `CADENCE_ALIAS` is passed through
/// `agent ready`). Claims stack FIFO — N claims release N sends.
struct Claim {
    at: Instant,
    by: Option<String>,
}

struct PtyState {
    /// tmux session name (the agent alias).
    session: String,
    /// Native Devin session id (lock filename stem), learned at open.
    native_session: String,
    /// Pane pid verified against the session lock.
    pane_pid: u32,
    /// Endpoint generation minted per `open`; embedded in tokens.
    generation: String,
    /// Single-use operator readiness claims, oldest first.
    claims: std::collections::VecDeque<Claim>,
}

pub struct DevinPtyAdapter {
    hooks: AdapterHooks,
    state: Mutex<PtyState>,
    socket: String,
    /// `~/.local/share/devin/cli/session_locks` — overridable in tests.
    locks_dir: PathBuf,
    /// Absolute `devin` argv resolved at construction.
    devin: String,
    /// `tmux` binary (env-overridable for tests).
    tmux: String,
    desired_session: Option<String>,
    cwd: String,
    /// Cadence state dir, exported into the pane for `cadence self`.
    state_dir: PathBuf,
    /// `params.auto_ready == "verified"`: the daemon probes the pane
    /// itself instead of requiring a human `agent ready` claim.
    /// Mutable — `agent set` refreshes it on the live adapter.
    auto_ready: AtomicBool,
}

fn short_hash(text: &str) -> String {
    // FNV-1a — deterministic, private socket per state dir.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

fn shlex_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Strip all whitespace so a body wrapped/indented by the TUI still
/// matches its source text contiguously.
fn normalize_screen(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The last `n` characters of `text` (by char, not byte).
fn tail_chars(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().skip(chars.len().saturating_sub(n)).collect()
}

fn resolve_on_path(bin: &str) -> Result<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = Path::new(dir).join(bin);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(Error::rejected(format!("`{bin}` not found on PATH")))
}

impl DevinPtyAdapter {
    pub fn new(hooks: AdapterHooks, log_path: &Path, agent: &Agent) -> Result<Self> {
        let state_dir = log_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let locks_dir = std::env::var("CADENCE_DEVIN_LOCKS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default())
                    .join(".local/share/devin/cli/session_locks")
            });
        let desired_session = agent
            .params
            .as_ref()
            .and_then(|p| p.get("session"))
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())
            .or_else(|| agent.thread_id.clone());
        Ok(Self {
            hooks,
            state: Mutex::new(PtyState {
                session: agent.alias.clone(),
                native_session: String::new(),
                pane_pid: 0,
                generation: String::new(),
                claims: std::collections::VecDeque::new(),
            }),
            socket: format!("cadence-{}", short_hash(&state_dir.to_string_lossy())),
            locks_dir,
            // Env override is an operator-provided command line used
            // verbatim (tests pass `python3 mock.py <dir>`); a real
            // `devin` found on PATH is shell-quoted for the pane shell.
            devin: match std::env::var("CADENCE_DEVIN_COMMAND") {
                Ok(cmd) if !cmd.is_empty() => cmd,
                _ => resolve_on_path("devin").map(|p| shlex_quote(&p))?,
            },
            tmux: std::env::var("CADENCE_TMUX_COMMAND")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "tmux".to_string()),
            desired_session,
            cwd: agent.cwd.clone(),
            state_dir,
            auto_ready: AtomicBool::new(
                agent
                    .params
                    .as_ref()
                    .and_then(|p| p.get("auto_ready"))
                    .and_then(|v| v.as_str())
                    == Some("verified"),
            ),
        })
    }

    fn tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        Ok(Command::new(&self.tmux)
            .arg("-L")
            .arg(&self.socket)
            .args(args)
            .output()?)
    }

    fn tmux_ok(&self, args: &[&str]) -> Result<String> {
        let out = self.tmux(args)?;
        if !out.status.success() {
            return Err(Error::provider(format!(
                "tmux {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn has_session(&self, session: &str) -> bool {
        self.tmux(&["has-session", "-t", session])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn pane_value(&self, session: &str, format: &str) -> Result<String> {
        self.tmux_ok(&["display-message", "-p", "-t", session, format])
    }

    fn pane_pid(&self, session: &str) -> Result<u32> {
        self.pane_value(session, "#{pane_pid}")?
            .parse()
            .map_err(|_| Error::internal("tmux returned a non-numeric pane_pid"))
    }

    /// Is `pid` the pane process or one of its descendants?
    fn descends_from(&self, mut pid: u32, pane_pid: u32) -> bool {
        let mut seen = std::collections::HashSet::new();
        while pid != 0 && seen.insert(pid) {
            if pid == pane_pid {
                return true;
            }
            let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
                return false;
            };
            pid = status
                .lines()
                .find_map(|l| l.strip_prefix("PPid:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
        }
        false
    }

    /// Every `/proc` pid holding an open fd to `lock`.
    fn lock_holders(&self, lock: &Path) -> Vec<u32> {
        let target = lock.to_string_lossy().into_owned();
        let mut holders = Vec::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return holders;
        };
        for proc in procs.flatten() {
            let Ok(pid) = proc.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(fds) = std::fs::read_dir(proc.path().join("fd")) else {
                continue;
            };
            let holds = fds.flatten().any(|fd| {
                std::fs::read_link(fd.path())
                    .map(|l| l.to_string_lossy() == target)
                    .unwrap_or(false)
            });
            if holds {
                holders.push(pid);
            }
        }
        holders
    }

    /// The native session whose lock is held by a descendant of
    /// `pane_pid`, or `None` when the pane owns no session.
    fn owned_lock(&self, pane_pid: u32) -> Option<String> {
        let entries = std::fs::read_dir(&self.locks_dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(session) = name.strip_suffix(".lock") else {
                continue;
            };
            if self
                .lock_holders(&entry.path())
                .iter()
                .any(|&pid| self.descends_from(pid, pane_pid))
            {
                return Some(session.to_string());
            }
        }
        None
    }

    fn lock_path(&self, session: &str) -> PathBuf {
        self.locks_dir.join(format!("{session}.lock"))
    }

    fn session(&self) -> String {
        self.state.lock().unwrap().session.clone()
    }

    fn session_and_native(&self) -> (String, String) {
        let s = self.state.lock().unwrap();
        (s.session.clone(), s.native_session.clone())
    }

    /// Verify the pane currently owns its native session lock.
    fn verify_ownership(&self, session: &str, native: &str) -> Result<u32> {
        let pane_pid = self.pane_pid(session)?;
        if self
            .lock_holders(&self.lock_path(native))
            .iter()
            .any(|&pid| self.descends_from(pid, pane_pid))
        {
            Ok(pane_pid)
        } else {
            Err(Error::provider(format!(
                "pane does not hold the Devin session lock for '{native}'"
            )))
        }
    }

    /// Gate evaluation before any paste: the pane must be live, not in a
    /// tmux mode, and still the lock owner — else the endpoint is dead.
    /// Then readiness: a fresh unconsumed operator claim always wins;
    /// without one, `auto_ready=verified` agents get a daemon-run screen
    /// probe (idle pane → self-claim, recorded as a `ready_claimed`
    /// event by `"daemon"`); anything else requeues for a retry.
    fn check_gate(&self, message_id: &str) -> Result<()> {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("tmux session is gone"));
        }
        if self.pane_value(&session, "#{pane_dead}")? == "1" {
            return Err(Error::provider("pane process has exited"));
        }
        self.verify_ownership(&session, &native)?;
        if self.pane_value(&session, "#{pane_in_mode}")? != "0" {
            return Err(Error::gate("pane is in a tmux mode (copy/view)"));
        }
        let claimed = {
            // Claims stack FIFO: drop expired heads, consume the oldest
            // fresh one — one paste per claim, always.
            let mut state = self.state.lock().unwrap();
            while let Some(front) = state.claims.front() {
                if front.at.elapsed() > READY_TTL {
                    state.claims.pop_front();
                } else {
                    break;
                }
            }
            state.claims.pop_front()
        };
        if let Some(claim) = claimed {
            // Which claim released this send is audit-relevant (G5):
            // the claimer is recorded at consumption, not just claim.
            (self.hooks.on_event)(
                "cadence/claim_used",
                serde_json::json!({
                    "message": message_id,
                    "by": claim.by.unwrap_or_else(|| "operator".to_string()),
                }),
            );
            return Ok(());
        }
        if !self.auto_ready.load(AtomicOrdering::SeqCst) {
            return Err(Error::gate(
                "no fresh `agent ready` claim — an operator must verify the \
                 terminal is idle with an empty input before submission",
            ));
        }
        let probe = self.probe()?;
        if probe.idle {
            (self.hooks.on_event)(
                "cadence/ready_claimed",
                serde_json::json!({"by": "daemon", "probe": probe.to_json()}),
            );
            return Ok(());
        }
        Err(Error::gate(format!("tui not idle: {}", probe.reason)))
    }

    /// Visible screen only (no scrollback) — what the TUI shows now.
    fn capture_visible(&self) -> Result<String> {
        let session = self.session();
        self.tmux_ok(&["capture-pane", "-p", "-t", &session])
    }
}

impl ProviderAdapter for DevinPtyAdapter {
    fn open(&self, _agent: &Agent) -> Result<Identity> {
        let (session, desired) = {
            let s = self.state.lock().unwrap();
            (s.session.clone(), self.desired_session.clone())
        };
        let generation = Uuid::new_v4().simple().to_string();

        let (native, pane_pid) = if self.has_session(&session) {
            // Reattach: verify the pane still owns a native session.
            // When one was recorded it must match; a pane left by a
            // crashed first open (nothing recorded yet) is adopted by
            // discovering which lock it holds.
            let pane_pid = self.pane_pid(&session)?;
            match (&desired, self.owned_lock(pane_pid)) {
                (Some(want), Some(found)) if *want == found => (found, pane_pid),
                (Some(want), Some(found)) => {
                    return Err(Error::provider(format!(
                        "pane owns session '{found}', expected '{want}' — \
                         changed owner fails closed"
                    )))
                }
                (Some(want), None) => {
                    return Err(Error::provider(format!(
                        "pane does not hold the Devin session lock for '{want}'"
                    )))
                }
                (None, Some(found)) => (found, pane_pid),
                (None, None) => {
                    return Err(Error::provider(
                        "pane exists but owns no Devin session lock",
                    ))
                }
            }
        } else {
            // Refuse takeover: a lock held outside our (future) pane
            // means another TUI already owns the native session.
            if let Some(want) = &desired {
                if let Some(foreign) = self.lock_holders(&self.lock_path(want)).first() {
                    return Err(Error::rejected(format!(
                        "Devin session '{want}' is locked by another terminal \
                         (pid {foreign}); close it first — no takeover"
                    )));
                }
            }
            let mut argv = self.devin.clone();
            if let Some(want) = &desired {
                argv.push_str(&format!(" -r {}", shlex_quote(want)));
            }
            // Keep a dead pane visible briefly instead of dropping to a
            // bare shell that would accept input meant for Devin.
            let command =
                format!("{argv}; printf '\\nDevin exited. This pane will close.\\n'; sleep 3");
            // Pane env identifies the agent to `cadence self`; -e args
            // are tmux options, never shell-interpreted.
            let env_alias = format!("CADENCE_ALIAS={session}");
            let env_dir = format!("CADENCE_STATE_DIR={}", self.state_dir.display());
            self.tmux_ok(&[
                "new-session",
                "-d",
                "-s",
                &session,
                "-c",
                &self.cwd,
                "-x",
                "120",
                "-y",
                "40",
                "-e",
                &env_alias,
                "-e",
                &env_dir,
                &command,
            ])?;
            let pane_pid = self.pane_pid(&session)?;
            // Bound the wait for the TUI to take its native session lock.
            let deadline = Instant::now() + OPEN_DEADLINE;
            let native = loop {
                if !self.has_session(&session) {
                    return Err(Error::provider("pane exited during TUI startup"));
                }
                if let Some(found) = self.owned_lock(pane_pid) {
                    if let Some(want) = &desired {
                        if found != *want {
                            return Err(Error::provider(format!(
                                "pane acquired session '{found}', expected '{want}'"
                            )));
                        }
                    }
                    break found;
                }
                if Instant::now() >= deadline {
                    return Err(Error::provider(
                        "timed out waiting for the Devin TUI to acquire its session lock",
                    ));
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            (native, pane_pid)
        };

        // Pane defaults for cadence-owned sessions, scoped to this
        // private tmux server — `-g`/`-gw` here never touch the user's
        // own tmux. Best effort: a cosmetic failure must not fence a
        // working endpoint.
        let _ = self.tmux_ok(&["set-option", "-g", "mouse", "on"]);
        let _ = self.tmux_ok(&["set-option", "-g", "set-clipboard", "on"]);
        let _ = self.tmux_ok(&["set-option", "-g", "status-left-length", "40"]);
        let _ = self.tmux_ok(&["set-option", "-gw", "pane-border-status", "top"]);
        let _ = self.tmux_ok(&[
            "set-option",
            "-gw",
            "pane-border-format",
            " #{session_name} ",
        ]);

        let endpoint = format!("tmux://{}/{session}", self.socket);
        {
            let mut s = self.state.lock().unwrap();
            s.native_session = native.clone();
            s.pane_pid = pane_pid;
            s.generation = generation.clone();
            s.claims.clear(); // a new endpoint can never inherit claims
        }
        Ok(Identity {
            thread_id: native.clone(),
            session_id: native,
            model: None,
            pid: pane_pid,
            endpoint: Some(endpoint),
            generation: Some(generation),
        })
    }

    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        // Literal-only content: pasted verbatim, so reject anything the
        // TUI could interpret as keys. These are `pre_write` rejections —
        // provably no bytes reached the pane, so the message fails
        // without fencing the endpoint.
        if prompt.is_empty() || prompt.len() > 4000 {
            return Err(Error::pre_write("PTY messages must be 1–4000 characters"));
        }
        if prompt.chars().any(|c| (c as u32) < 32 || c as u32 == 127) {
            return Err(Error::pre_write(
                "PTY messages must be a single line without control characters",
            ));
        }
        self.check_gate(client_message_id)?;

        let token = {
            let s = self.state.lock().unwrap();
            format!("pty-{}-{}", s.generation, Uuid::new_v4().simple())
        };
        let session = self.session();

        // The render check is *differential*: "this paste added text",
        // not "the text is somewhere on screen". Capture before the
        // paste so an identical earlier body (a repeated routed
        // notification, a re-sent task) cannot pass for this one.
        let before = self.capture_visible()?;
        // The probe slice is the body's normalized tail — the end is
        // what stays visible on a horizontally-scrolled input line and
        // what a wrapped transcript renders last. Whitespace is stripped
        // on both sides so TUI line wrapping/indentation cannot hide a
        // match. For routed `worker_result` bodies the tail covers the
        // unique worker turn_id; for repeated plain text the
        // occurrence-count delta is still differential.
        let slice = normalize_screen(&tail_chars(prompt, PROBE_SLICE));
        let before_count = normalize_screen(&before).matches(&slice).count();

        // Literal delivery: content travels in a tmux buffer file, never
        // through argv or a shell — quoting cannot corrupt or inject it.
        let mut tmp = tempfile::NamedTempFile::new()?;
        tmp.write_all(prompt.as_bytes())?;
        tmp.flush()?;
        let path = tmp.path().to_string_lossy().into_owned();
        self.tmux_ok(&["load-buffer", "-b", BUFFER, &path])?;
        if self
            .tmux(&["paste-buffer", "-d", "-p", "-b", BUFFER, "-t", &session])?
            .status
            .success()
        {
            std::thread::sleep(PASTE_SETTLE);
            // Enter is what may commit the turn — a failure after the
            // paste is ambiguous: the text could already be submitted.
            self.tmux_ok(&["send-keys", "-t", &session, "Enter"])
                .map_err(|e| Error::unknown(format!("post-paste submit failed: {e}")))?;
        } else {
            return Err(Error::unknown(
                "paste-buffer failed after content reached the terminal path",
            ));
        }
        drop(tmp);

        // Post-paste verification, bounded by RENDER_DEADLINE: the
        // slice's occurrence count must increase AND the input line must
        // be empty again — text rendered but still sitting in the input
        // means Enter never submitted (a staged draft is not a turn).
        // A miss inside the bound is evidence of a dropped paste, never
        // proof; the daemon decides per message kind what a miss means.
        let deadline = Instant::now() + RENDER_DEADLINE;
        let mut rendered = false;
        loop {
            let screen = self.capture_visible()?;
            if normalize_screen(&screen).matches(&slice).count() > before_count {
                rendered = true;
                if !analyze_devin(&screen).input_nonempty {
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::not_rendered(if rendered {
                    "paste rendered in the input line but was never submitted — \
                     Enter not observed; the draft is left untouched"
                } else {
                    "pasted text never rendered in the pane — the TUI dropped it"
                }));
            }
            std::thread::sleep(Duration::from_millis(150));
        }

        on_started(&token);
        Ok(TurnResult {
            turn_id: token,
            status: "submitted".to_string(),
            text: String::new(),
            stop_reason: None,
            error: None,
        })
    }

    fn respond(&self, _request_id: &Value, _result: Value) -> Result<()> {
        Err(Error::rejected(
            "pty endpoints have no approval channel — answer Devin \
             permission prompts in the terminal itself",
        ))
    }

    fn interrupt(&self) {
        let session = self.session();
        let _ = self.tmux(&["send-keys", "-t", &session, "C-c"]);
    }

    fn disconnected(&self) -> bool {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return true;
        }
        let dead = self
            .pane_value(&session, "#{pane_dead}")
            .map(|v| v == "1")
            .unwrap_or(true);
        // A live pane that lost the native lock is not our endpoint.
        dead || self.verify_ownership(&session, &native).is_err()
    }

    fn close(&self) {
        let session = self.session();
        // Only ever kills a session on our own private socket — one we
        // launched. Foreign panes are never registered as killable.
        let _ = self.tmux(&["kill-session", "-t", &session]);
    }

    /// Daemon shutdown must not kill the visible pane: the TUI belongs
    /// to the operator's screen and survives for reattach on restart.
    fn detach(&self) {}

    fn claim_ready(&self, by: Option<String>) -> Result<()> {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("cannot claim readiness: pane is gone"));
        }
        self.verify_ownership(&session, &native)?;
        let mut state = self.state.lock().unwrap();
        if state.claims.len() >= CLAIM_CAPACITY {
            state.claims.pop_front();
        }
        state.claims.push_back(Claim {
            at: Instant::now(),
            by,
        });
        Ok(())
    }

    fn capture(&self) -> Result<String> {
        let session = self.session();
        self.tmux_ok(&["capture-pane", "-p", "-t", &session, "-S", "-120"])
    }

    fn probe(&self) -> Result<Probe> {
        Ok(analyze_devin(&self.capture_visible()?))
    }

    fn update_params(&self, params: &Value) {
        self.auto_ready.store(
            params.get("auto_ready").and_then(|v| v.as_str()) == Some("verified"),
            AtomicOrdering::SeqCst,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{analyze_devin, normalize_screen, tail_chars, STATUS_LINES};

    /// Real idle screen captured from a live Devin pane.
    const IDLE: &str = "\
❭ Ask Devin to build features, fix bugs, or work on your code
──────────────────────────────────────────────────────────────────
SWE-2 Max                                          Context: 43k / 262k";

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
    fn busy_markers_block_even_with_prompt() {
        for marker in [
            "(esc twice to interrupt)",
            "Cancel agent (esc twice)",
            "Guide Devin while it works",
            "Press Ctrl+O to view the full thinking trace",
        ] {
            let screen = format!("{IDLE}\nWorking on it {marker}");
            let p = analyze_devin(&screen);
            assert!(!p.idle && p.busy_marker, "{marker}: {}", p.reason);
        }
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
        // more to it is still unsafe.
        let screen = format!("{IDLE}\nPress Enter to send queued messages now");
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
        let p = analyze_devin(&screen);
        assert!(p.idle, "{} / {}", p.idle, p.reason);
        assert!(!p.busy_marker && !p.approval_menu);
    }

    #[test]
    fn markers_in_status_region_still_block() {
        // Same strings inside the bottom region DO mean busy — the
        // region is the TUI's live status/menu area.
        let screen = format!("{IDLE}\n⠀⠇ Thinking · 30s (esc twice to interrupt)");
        let p = analyze_devin(&screen);
        assert!(!p.idle && p.busy_marker);
    }

    #[test]
    fn normalize_and_tail_helpers() {
        assert_eq!(normalize_screen("a b\n  c"), "abc");
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 9), "ab");
        // A wrapped body still matches its own tail slice.
        let body = "alpha beta gamma delta omega";
        let rendered = "alpha beta\n    gamma delta\n    omega";
        let slice = normalize_screen(&tail_chars(body, 12));
        assert!(normalize_screen(rendered).contains(&slice));
    }
}
