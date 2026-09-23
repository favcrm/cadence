//! A provider terminal UI driven through an owned tmux session,
//! parameterised by a [`TuiProfile`].
//!
//! The daemon launches the provider's TUI inside a detached tmux
//! session on a private socket (`cadence-<state-hash>`), so every pane
//! it may kill is one it spawned. Native session ownership is *proven*,
//! not assumed: how ownership is proven is the profile's business —
//! [`TuiProfile::owned_session`]/[`TuiProfile::verify_ownership`] — and
//! a session held by a foreign process is a refusal, never a takeover.
//!
//! Submission is gated, never blind: every `run_turn` re-verifies the
//! pane is alive, unblocked (`pane_in_mode == 0`), still owns its
//! native session, and consumes a readiness claim. Claims are
//! single-use, time-boxed ([`READY_TTL`]) and stack FIFO — N claims
//! release N queued sends, each attributed to its claimer for the audit
//! record. Agents opted into `params.auto_ready = "verified"` let the
//! daemon mint the claim itself after a screen probe
//! ([`TuiProfile::analyze`]) proves the pane idle; a human `agent
//! ready` still wins whenever both exist. A routed notice
//! (`worker_result`, `worker_notice`, `job_event`) may also paste
//! into an idle pane with no claim and with `auto_ready` off — the
//! actor sets that for the duration of one `run_turn` only. A busy
//! pane or an open approval menu still refuses.
//!
//! Text is delivered literally through a tmux buffer (`load-buffer` +
//! bracketed `paste-buffer -p` + `Enter`); no shell interpolation and no
//! control characters — and a body whose first non-space character is
//! in the profile's [`TuiProfile::forbidden_prefixes`] list is rejected
//! `PreWrite` before any byte reaches the pane, because TUIs commonly
//! treat a leading `/`/`!`-style character as a command or mode switch
//! and a verbatim paste of one is an injection path. After Enter, a
//! differential render check must see this paste's text newly on the
//! visible screen *and* the input line empty again within
//! [`RENDER_DEADLINE`] — a busy TUI drops a bracketed paste silently,
//! so bytes-sent is not delivery evidence. A miss inside the bound is
//! `NotRendered` — evidence, not proof, which is why the daemon parks
//! exhausted informational deliveries and fences task messages as
//! `unknown` rather than failing them.
//!
//! Terminal echo proves *rendering*, never model receipt. A pasted
//! message stays `running` under its `pty-<generation>-<uuid>` token
//! until an explicit `message ack` / `message result` report completes
//! it; tokens from a previous endpoint generation are rejected. If the
//! endpoint dies after a possible paste the outcome is `unknown` and
//! the attempt is never replayed.
//!
//! Approval prompts are not brokered: `agent respond` is rejected for
//! this endpoint — the profile supplies the message naming the
//! provider's prompt.

pub mod claude;
pub mod cursor;
pub mod devin;
pub mod lane;
pub mod profile;
mod render;
pub mod sgr;
pub mod stub;

pub use claude::{analyze_claude, analyze_claude_styled, ClaudeProfile};
pub use cursor::{analyze_cursor, CursorProfile};
pub use devin::{analyze_devin, DevinProfile};
pub use profile::TuiProfile;
pub use stub::StubProfile;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::adapter::registry;
use crate::error::{Error, Result};
use crate::store::Agent;

use super::{AdapterHooks, Identity, Probe, ProviderAdapter, ProviderEnv, TurnResult};
use render::{RenderDecision, RenderObservation, RenderOutcome};

/// How long an operator readiness claim stays valid for one send.
const READY_TTL: Duration = Duration::from_secs(60);
/// Pause between bracketed paste and Enter so the TUI consumes it.
const PASTE_SETTLE: Duration = Duration::from_millis(300);
/// Bounded post-paste wait for the body to render in the transcript.
/// A miss inside the bound is *evidence* of a dropped paste, not proof
/// — a saturated host renders late — which is why a task message lands
/// `unknown` (uncertainty discipline) rather than `failed`.
const RENDER_DEADLINE: Duration = Duration::from_secs(4);
/// Slice of the pasted body used for the differential render check.
/// The tail is what stays visible: a long input scrolls horizontally
/// to the cursor, and a wrapped transcript ends with it.
const PROBE_SLICE: usize = 64;
/// Stacked operator claims retained for the queue (oldest dropped past
/// this); each claim releases exactly one gated message.
const CLAIM_CAPACITY: usize = 16;
const BUFFER: &str = "cadence-msg";

/// One operator readiness claim: single-use, time-boxed, attributed to
/// the claimer when known (the pane's `CADENCE_ALIAS` is passed through
/// `agent ready`). Claims stack FIFO — N claims release N sends.
struct Claim {
    at: Instant,
    by: Option<String>,
    /// The screen probe that admitted the claim — recorded so a later
    /// `paste_not_rendered` can show what "idle" looked like at claim
    /// time. Forced claims keep the busy verdict they overrode.
    probe: Probe,
}

struct PtyState {
    /// tmux session name (the agent alias).
    session: String,
    /// Native TUI session id, learned at open.
    native_session: String,
    /// Pane pid verified against the profile's ownership proof.
    pane_pid: u32,
    /// Endpoint generation minted per `open`; embedded in tokens.
    generation: String,
    /// Single-use operator readiness claims, oldest first.
    claims: std::collections::VecDeque<Claim>,
    /// Consecutive failed `disconnected()` probes — the pane is only
    /// believed dead after `DISCONNECT_MISS_BUDGET` misses across
    /// idle ticks (never inside one tick — see `disconnected`).
    disconnected_misses: usize,
    /// The probe verdict that admitted the in-flight send — the claim's
    /// own verdict for an operator claim, the just-run probe for a
    /// daemon auto-claim. Carried on `NotRendered` evidence.
    gate_probe: Option<Probe>,
}

/// The generic adapter: tmux mechanics, readiness claims and the
/// differential render check, with every provider-specific fact behind
/// `profile`.
pub struct PtyAdapter {
    hooks: AdapterHooks,
    state: Mutex<PtyState>,
    socket: String,
    /// `tmux` binary (env-overridable for tests).
    tmux: String,
    desired_session: Option<String>,
    cwd: String,
    /// Cadence state dir, exported into the pane for `cadence self`.
    state_dir: PathBuf,
    /// The daemon's tracker and profile, exported into the pane
    /// ([`crate::adapter::DAEMON_CONTEXT_ENV`]).
    context_env: Vec<(String, String)>,
    /// `params.auto_ready == "verified"`: the daemon probes the pane
    /// itself instead of requiring a human `agent ready` claim.
    /// Mutable — `agent set` refreshes it on the live adapter.
    auto_ready: AtomicBool,
    /// Set by the actor for one `run_turn` when the message is routed.
    /// With no claim and `auto_ready` off, an idle probe may still
    /// admit the paste. Cleared when the turn returns.
    unclaimed_ok: AtomicBool,
    /// Serialises probe→input sequences that must not interleave: the
    /// send gate's probe→paste→Enter and `agent answer`'s
    /// probe→send-keys. Without it a menu closing between the answer's
    /// capture and its keystroke would land a digit as draft text.
    paste_lock: Mutex<()>,
    /// The provider TUI this pane runs.
    profile: Box<dyn TuiProfile>,
}

/// The pane's `-e` environment: the agent's alias and state dir for
/// `cadence self`, then the daemon's tracker and profile — a sandbox
/// worker's `cadence issue …` must stay in the sandbox (CAD-310).
fn pane_env(alias: &str, state_dir: &Path, context: &[(String, String)]) -> Vec<String> {
    let mut vars = vec![
        format!("CADENCE_ALIAS={alias}"),
        format!("CADENCE_STATE_DIR={}", state_dir.display()),
    ];
    vars.extend(context.iter().map(|(k, v)| format!("{k}={v}")));
    vars
}

fn short_hash(text: &str) -> String {
    // FNV-1a — deterministic, private socket per state dir.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

pub(crate) fn shlex_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\\''"))
}

/// Inline markdown markers a TUI may render away when it echoes a
/// submitted prompt: Claude shows the raw body in its input box but
/// renders the transcript copy as markdown, so `` `cadence self` ``
/// comes back as `cadence self` (CAD-282).
const MARKDOWN_MARKERS: [char; 4] = ['`', '*', '_', '~'];

/// Strip all whitespace so a body wrapped/indented by the TUI still
/// matches its source text contiguously, and the inline markdown
/// markers so a raw input-box echo and a rendered transcript echo both
/// match. Applied to the slice and the screen alike, so the count stays
/// differential.
fn normalize_screen(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !MARKDOWN_MARKERS.contains(c))
        .collect()
}

/// The last `n` characters of `text` (by char, not byte).
fn tail_chars(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().skip(chars.len().saturating_sub(n)).collect()
}

/// The last `rows` lines of a screen capture — control characters
/// stripped, lines right-trimmed, trailing blanks dropped. The
/// normalized tail a `paste_not_rendered` event carries so a fenced
/// paste shows what the pane actually displayed.
fn screen_tail(text: &str, rows: usize) -> Vec<String> {
    let mut lines: Vec<String> = text
        .lines()
        .map(|l| {
            l.chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(rows);
    lines.into_iter().skip(skip).collect()
}

/// Lines of normalized tail the stall hash covers — recent content
/// only, so scrollback shifting identical text never reads as change.
const ACTIVITY_LINES: usize = 24;

/// A capture that differs only in ticking status artifacts hashes
/// identically: control characters are stripped, whitespace
/// collapses, spinner/bullet glyphs and elapsed-time counters
/// (`· 2m 15s`, `(1m 2s)`, `83%`, `12:34`) drop out, and only the
/// newest `ACTIVITY_LINES` lines count. The daemon's stall watch
/// samples this on a bounded interval — a "still working" footer
/// re-rendering its clock is not provider activity.
pub fn activity_hash(screen: &str) -> String {
    use sha2::{Digest, Sha256};
    let lines: Vec<String> = screen
        .lines()
        .map(|l| {
            l.chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .split_whitespace()
                .filter(|tok| activity_token(tok))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect();
    let skip = lines.len().saturating_sub(ACTIVITY_LINES);
    let mut h = Sha256::new();
    for l in &lines[skip..] {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    format!("{:x}", h.finalize())
}

/// Keep a screen word unless it is a ticking artifact: tokens with no
/// letter or digit (spinner/braille glyphs, box rules, a bare `·`),
/// clock faces (`12:34`, `12:34:56`), and elapsed-time/counter forms
/// (`2m`, `15s`, `150ms`, `3h`, `83%`, `128k`). The words around a
/// spinner still count, so a status line that actually changes reads
/// as activity.
fn activity_token(tok: &str) -> bool {
    if !tok.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let t = tok.trim_matches(|c: char| !c.is_alphanumeric());
    if t.contains(':')
        && t.split(':')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    for suffix in ["ms", "s", "m", "h", "%", "k"] {
        if let Some(body) = t.strip_suffix(suffix) {
            return body.is_empty() || !body.chars().all(|c| c.is_ascii_digit() || c == '.');
        }
    }
    true
}

/// This state dir's private tmux server socket name (`tmux -L`).
pub fn tmux_socket(state_dir: &Path) -> String {
    format!("cadence-{}", short_hash(&state_dir.to_string_lossy()))
}

/// Kill this state dir's private tmux server and every pane on it,
/// returning how many sessions it still held. A daemon stop keeps pty
/// panes for a hot restart; a sandbox going `down` or `reset` never
/// gets one (CAD-310). Best effort: no server is already the goal.
pub(crate) fn kill_server(state_dir: &Path, env: &ProviderEnv) -> usize {
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = tmux_socket(state_dir);
    let sessions = crate::reaper::output(Command::new(&tmux).arg("-L").arg(&socket).args([
        "list-sessions",
        "-F",
        "#{session_name}",
    ]))
    .ok()
    .filter(|out| out.status.success())
    .map(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
    })
    .unwrap_or(0);
    if sessions > 0 {
        let _ = crate::reaper::output(
            Command::new(&tmux)
                .arg("-L")
                .arg(&socket)
                .arg("kill-server"),
        );
    }
    sessions
}

/// Kill `alias`'s session on this state dir's private tmux socket —
/// the explicit kill path for `agent stop`/`remove`/`gc` on a pty
/// agent whose pane may have survived a fence (fences detach now).
/// Best effort: only sessions we launched exist on this socket, and a
/// missing session or server is already the goal state.
pub(crate) fn kill_pane(state_dir: &Path, alias: &str, env: &ProviderEnv) {
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = format!("cadence-{}", short_hash(&state_dir.to_string_lossy()));
    let _ = crate::reaper::output(Command::new(tmux).arg("-L").arg(&socket).args([
        "kill-session",
        "-t",
        alias,
    ]));
}

/// Whether `alias` still has a session on this state dir's private
/// tmux socket. The CAD-199 agent-gc timer is records-only — it never
/// kills — so it keeps any pty row whose pane survived a fence rather
/// than orphan that pane. A `tmux` that cannot run counts as alive:
/// fail closed, keep the row.
pub(crate) fn pane_alive(state_dir: &Path, alias: &str, env: &ProviderEnv) -> bool {
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = format!("cadence-{}", short_hash(&state_dir.to_string_lossy()));
    crate::reaper::output(Command::new(tmux).arg("-L").arg(&socket).args([
        "has-session",
        "-t",
        &format!("={alias}"),
    ]))
    .map_or(true, |out| out.status.success())
}

/// CAD-96: how many terminal clients are attached to `alias`'s session
/// on this state dir's private tmux socket (`cadence attach`, or a
/// hand-run `tmux attach`). `None` when tmux cannot answer — the idle
/// auto-stop timer then keeps the agent: fail closed.
pub(crate) fn pane_clients(state_dir: &Path, alias: &str, env: &ProviderEnv) -> Option<usize> {
    let tmux = env
        .var("CADENCE_TMUX_COMMAND")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "tmux".to_string());
    let socket = format!("cadence-{}", short_hash(&state_dir.to_string_lossy()));
    let out = crate::reaper::output(Command::new(tmux).arg("-L").arg(&socket).args([
        "list-clients",
        "-t",
        &format!("={alias}"),
        "-F",
        "#{client_tty}",
    ]))
    .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}

pub(crate) fn resolve_on_path(bin: &str) -> Result<String> {
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let candidate = Path::new(dir).join(bin);
        if candidate.is_file() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(Error::rejected(format!("`{bin}` not found on PATH")))
}

/// Consecutive `disconnected()` misses before the pane is believed
/// dead — counted across idle ticks so no single tick pays for a
/// re-probe (see `disconnected`).
const DISCONNECT_MISS_BUDGET: usize = 3;

/// Is `pid` the pane process or one of its descendants? Single-shot:
/// callers that need resilience retry at their own decision point.
pub(crate) fn descends_from(mut pid: u32, pane_pid: u32) -> bool {
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

/// The process's /proc ancestry chain — itself first, then each PPid
/// link up to (excluding) init. Fail-closed for connection-bound
/// caller identity (CAD-113): an unreadable or malformed link yields
/// `None`, never a partial chain — a caller whose ancestry cannot be
/// verified must inherit no identity at all. A detached caller
/// (`setsid`) reparents to init — or, under `daemon run`, to the daemon,
/// the child subreaper of everything it launched (CAD-308) — so no
/// registered pane is left on its chain.
pub(crate) fn caller_chain(mut pid: u32) -> Option<Vec<u32>> {
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    while pid > 1 && seen.insert(pid) {
        chain.push(pid);
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
        pid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|v| v.trim().parse().ok())?;
    }
    Some(chain)
}

/// The pty paste ceiling in bytes: a body longer than this fails
/// pre-write. Kickoffs and task-bound messages are composed to fit it —
/// prose gives way, acceptance criteria never do (CAD-160).
pub const MAX_BODY: usize = 4000;

/// Whether `text` holds a character the literal paste refuses: any C0
/// control (newline included) or DEL. Shared by the adapter's pre-write
/// check and the send/dispatch paths that refuse before enqueue.
pub fn has_control_chars(text: &str) -> bool {
    text.chars().any(|c| (c as u32) < 32 || c as u32 == 127)
}

/// The registered pane a caller descends from, given its
/// [`caller_chain`] and the live pane map (pane pid → alias): the
/// NEAREST pane on the chain wins, so a caller's own pane beats any
/// outer one and resolution never depends on map order. The daemon's
/// slot identity (CAD-113) resolves through here; pane-attention and
/// board-write identity use the wider [`crate::peer::PeerTies`] rule.
pub(crate) fn nearest_pane<'a>(
    chain: &[u32],
    panes: &'a std::collections::HashMap<u32, String>,
) -> Option<&'a String> {
    chain.iter().find_map(|pid| panes.get(pid))
}

/// A pty provider's forbidden input prefixes — the profile's own list,
/// surfaced here so the briefing can warn without constructing a
/// profile. Unknown providers get an empty list (no hazard asserted).
pub fn forbidden_prefixes(provider: &str) -> &'static [char] {
    match provider {
        "devin" => devin::FORBIDDEN_PREFIXES,
        "claude" => claude::FORBIDDEN_PREFIXES,
        "cursor" => cursor::FORBIDDEN_PREFIXES,
        "tui-stub" => stub::FORBIDDEN_PREFIXES,
        _ => &[],
    }
}

/// Every `/proc` pid holding an open fd to `lock`. Single-shot:
/// callers that need resilience retry at their own decision point.
pub(crate) fn lock_holders(lock: &Path) -> Vec<u32> {
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

impl PtyAdapter {
    pub fn new(
        hooks: AdapterHooks,
        log_path: &Path,
        agent: &Agent,
        env: &ProviderEnv,
        profile: impl TuiProfile + 'static,
    ) -> Result<Self> {
        let state_dir = log_path
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
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
                disconnected_misses: 0,
                gate_probe: None,
            }),
            socket: format!("cadence-{}", short_hash(&state_dir.to_string_lossy())),
            tmux: env
                .var("CADENCE_TMUX_COMMAND")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "tmux".to_string()),
            desired_session,
            cwd: agent.cwd.clone(),
            state_dir,
            context_env: crate::adapter::daemon_context_env(env),
            auto_ready: AtomicBool::new(
                agent
                    .params
                    .as_ref()
                    .and_then(|p| p.get("auto_ready"))
                    .and_then(|v| v.as_str())
                    == Some("verified"),
            ),
            unclaimed_ok: AtomicBool::new(false),
            paste_lock: Mutex::new(()),
            profile: Box::new(profile),
        })
    }

    fn tmux(&self, args: &[&str]) -> Result<std::process::Output> {
        Ok(crate::reaper::output(
            Command::new(&self.tmux)
                .arg("-L")
                .arg(&self.socket)
                .args(args),
        )?)
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

    fn session(&self) -> String {
        self.state.lock().unwrap().session.clone()
    }

    fn session_and_native(&self) -> (String, String) {
        let s = self.state.lock().unwrap();
        (s.session.clone(), s.native_session.clone())
    }

    /// Verify the pane currently owns its native session — the proof
    /// itself is the profile's. Single-shot: callers that need
    /// resilience retry at their own decision point.
    fn verify_ownership(&self, session: &str, native: &str) -> Result<u32> {
        let pane_pid = self.pane_pid(session)?;
        self.profile.verify_ownership(native, pane_pid)?;
        Ok(pane_pid)
    }

    /// Poll the profile's ownership proof until the pane claims a
    /// native session or the open deadline passes — the bounded wait
    /// both `open` branches share. `None` means the deadline elapsed
    /// with the pane alive but still holding no provable session; the
    /// caller applies `resolve_session`'s usual errors for that.
    fn wait_owned_session(&self, session: &str, pane_pid: u32) -> Result<Option<String>> {
        let deadline = Instant::now() + self.profile.open_deadline();
        loop {
            if !self.has_session(session) {
                return Err(Error::provider("pane exited during TUI startup"));
            }
            if let Some(found) = self.profile.owned_session(pane_pid) {
                return Ok(Some(found));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// One liveness probe: the tmux session exists, the pane is not
    /// dead, and it still owns its native session.
    fn pane_alive(&self) -> bool {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return false;
        }
        let dead = self
            .pane_value(&session, "#{pane_dead}")
            .map(|v| v == "1")
            .unwrap_or(true);
        // A live pane that lost its native session is not our endpoint.
        !dead && self.verify_ownership(&session, &native).is_ok()
    }

    /// Gate evaluation before any paste: the pane must be live, not in a
    /// tmux mode, and still the session owner — else the endpoint is
    /// dead. Then the screen probe — run before any claim is consumed,
    /// because an open approval menu is never idle and no claim may
    /// carry a paste past one (the refusal also costs nothing: no
    /// claim eaten). Readiness last: a fresh unconsumed operator claim
    /// always wins; without one, `auto_ready=verified` agents get the
    /// probe verdict (idle pane → self-claim, recorded as a
    /// `ready_claimed` event by `"daemon"`). A routed notice with the
    /// actor's unclaimed flag set takes the same idle-probe path when
    /// `auto_ready` is off, recorded with `"reason": "routed"`. A
    /// non-idle probe, an approval menu, or a user message with no
    /// claim still requeues.
    fn check_gate(&self, message_id: &str) -> Result<()> {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("tmux session is gone"));
        }
        if self.pane_value(&session, "#{pane_dead}")? == "1" {
            return Err(Error::provider("pane process has exited"));
        }
        let pane_pid = self.verify_ownership(&session, &native)?;
        // CAD-202: a pane whose cwd was deleted (its worktree removed
        // under it) runs work nowhere — refuse like any other gate, so
        // the message stays queued, before a claim is consumed.
        if let Some(cwd) = lane::pane_cwd(pane_pid).filter(|c| c.deleted) {
            return Err(Error::gate(format!(
                "cwd_deleted: the pane's working directory {} was deleted — \
                 re-home the lane (`cadence agent stop`, fix its cwd, resume) \
                 before delivery",
                cwd.path
            )));
        }
        if self.pane_value(&session, "#{pane_in_mode}")? != "0" {
            return Err(Error::gate("pane is in a tmux mode (copy/view)"));
        }
        // A transient `capture-pane` failure must retry like any other
        // gate refusal — a provider error here would fail the message
        // and fence the agent on a flake.
        let probe = self
            .probe()
            .map_err(|e| Error::gate(format!("pane probe failed: {e}")))?;
        if probe.approval_menu {
            return Err(Error::gate(format!(
                "approval menu is open: {} — answer it in the pane or with \
                 `cadence agent answer {} <choice>`",
                probe.reason, session
            )));
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
            // The claim's own probe verdict rides along — it is the
            // "idle" the sender believed in if the paste never renders.
            self.state.lock().unwrap().gate_probe = Some(claim.probe.clone());
            (self.hooks.on_event)(
                "cadence/claim_used",
                serde_json::json!({
                    "message": message_id,
                    "by": claim.by.unwrap_or_else(|| "operator".to_string()),
                }),
            );
            return Ok(());
        }
        let auto = self.auto_ready.load(AtomicOrdering::SeqCst);
        let routed = self.unclaimed_ok.load(AtomicOrdering::SeqCst);
        // A user or task paste still needs a claim or verified
        // auto-ready. A routed notice may proceed to the idle probe
        // with neither. The probe itself already refused a dead pane,
        // a tmux mode, and an open approval menu.
        if !auto && !routed {
            return Err(Error::gate(
                "no fresh `agent ready` claim — an operator must verify the \
                 terminal is idle with an empty input before submission",
            ));
        }
        if !probe.idle {
            return Err(Error::gate(format!("tui not idle: {}", probe.reason)));
        }
        self.state.lock().unwrap().gate_probe = Some(probe.clone());
        let mut payload = serde_json::json!({"by": "daemon", "probe": probe.to_json()});
        // Distinguish a routed idle paste from verified auto-ready.
        // When both are set, verified auto-ready is the recorded path.
        if routed && !auto {
            payload["reason"] = serde_json::json!("routed");
        }
        (self.hooks.on_event)("cadence/ready_claimed", payload);
        Ok(())
    }

    /// Visible screen only (no scrollback) — what the TUI shows now,
    /// as plain text.
    fn capture_visible(&self) -> Result<String> {
        Ok(sgr::strip(&self.capture_visible_styled()?))
    }

    /// Visible screen with its SGR attributes kept (`-e`) — what every
    /// screen probe reads. A plain capture cannot tell a TUI's dim
    /// ghost text (Claude's prompt suggestion) from a typed draft; the
    /// profile's [`TuiProfile::analyze_styled`] can. One capture feeds
    /// both the probe and any plain-text use, so they see one frame.
    fn capture_visible_styled(&self) -> Result<String> {
        let session = self.session();
        self.tmux_ok(&["capture-pane", "-p", "-e", "-t", &session])
    }

    /// CAD-201: record the pane root's process identity for this
    /// endpoint generation — the `pane_root` event `agent stop` reaps
    /// the pane's session by. An unreadable root is recorded as such:
    /// the tree is then unowned, never guessed at.
    fn record_pane_root(&self, pane_pid: u32, generation: &str) {
        match lane::PaneRoot::capture(pane_pid, generation) {
            Some(root) => (self.hooks.on_event)("cadence/pane_root", root.to_json()),
            None => (self.hooks.on_event)(
                "cadence/pane_root_unrecorded",
                serde_json::json!({"pid": pane_pid, "generation": generation,
                                   "reason": "pane root /proc/<pid>/stat unreadable at open"}),
            ),
        }
    }

    /// The pane cursor cell `(x, y)` for the analyzer — profiles use it
    /// to tell ghost suggestion text (never moves the cursor) from a
    /// real staged draft. `None` when unreadable — the analyzer then
    /// treats any visible draft as real.
    fn cursor_pos(&self, session: &str) -> Option<(u32, u32)> {
        let v = self.pane_value(session, "#{cursor_x},#{cursor_y}").ok()?;
        let (x, y) = v.split_once(',')?;
        Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
    }
}

impl ProviderAdapter for PtyAdapter {
    fn open(&self, _agent: &Agent) -> Result<Identity> {
        let (session, desired) = {
            let s = self.state.lock().unwrap();
            (s.session.clone(), self.desired_session.clone())
        };
        let generation = Uuid::new_v4().simple().to_string();

        // A stored session id means this open is a resume attempt, and
        // some failures prove the stored id can never resume: the pane
        // exits on the dead chat, or stays up but never acquires it.
        // The event lets the daemon drop the stored id so the next
        // open mints fresh — but only for profiles whose sessions are
        // disposable (a transient proof failure must never drop an
        // operator-supplied Claude/Devin session), and never on a
        // mismatch, where the pane's session could be a foreign one
        // the alias must keep refusing. Returns the reported id so the
        // caller can name it in the failure it hands back.
        let resume_failed = |desired: &Option<String>, reason: &Error| -> Option<String> {
            if !self.profile.session_is_disposable() {
                return None;
            }
            let session = desired.clone()?;
            (self.hooks.on_event)(
                "cadence/session_resume_failed",
                serde_json::json!({"session": session, "reason": reason.to_string()}),
            );
            Some(session)
        };
        // The clear lands silently in `params` — the operator-visible
        // copy is the returned error, which `agent.error` keeps until
        // the next successful open.
        let cleared_err = |e: Error, cleared: Option<String>| match cleared {
            Some(old) => Error::provider(format!(
                "{e} — stored session '{old}' was cleared; \
                 the next open mints a fresh chat"
            )),
            None => e,
        };

        let (native, pane_pid, attach) = if self.has_session(&session) {
            // Reattach: verify the pane still owns a native session.
            // When one was recorded it must match; a pane left by a
            // crashed first open (nothing recorded yet) is adopted by
            // discovering which session it owns. The proof is not
            // necessarily visible the instant the pane is — a provider
            // mid-registration, or a `/proc` scan that raced, both read
            // as "no session yet" — so the reattach waits on the same
            // deadline the spawn path gets rather than fencing a live
            // pane on one observation.
            let pane_pid = self.pane_pid(&session)?;
            // The pane was already running — an exit mid-wait is a
            // crash, not proof the stored chat is gone. The spawn
            // arm re-runs the resume on the next open and reports
            // a genuinely dead chat there.
            let found = self.wait_owned_session(&session, pane_pid)?;
            let found_any = found.is_some();
            match self.profile.resolve_session(desired.as_deref(), found) {
                Ok(native) => (native, pane_pid, "adopted"),
                Err(e) => {
                    // found=None: the pane holds nothing for
                    // the stored id — the resume never
                    // materialized, the chat is gone. A
                    // mismatch (found≠want) stays fail-closed
                    // and never clears.
                    let cleared = if found_any {
                        None
                    } else {
                        resume_failed(&desired, &e)
                    };
                    return Err(cleared_err(e, cleared));
                }
            }
        } else {
            // Mint once: a profile with a prepare step mints its native
            // session id *before* the pane exists, and the event folds
            // it into `params.session` — a respawn after a failed launch
            // resumes the same id instead of abandoning a mint per
            // retry. `desired` is the adapter's copy for this open.
            let resuming = desired.is_some();
            let desired = match desired {
                Some(want) => Some(want),
                None => {
                    let minted = self.profile.prepare_session()?;
                    if let Some(id) = &minted {
                        (self.hooks.on_event)(
                            "cadence/session_minted",
                            serde_json::json!({"session": id}),
                        );
                    }
                    minted
                }
            };
            // Refuse takeover: a session owned outside our (future)
            // pane means another TUI already owns it.
            if let Some(want) = &desired {
                self.profile.refuse_takeover(want)?;
            }
            let argv = self.profile.launch_command(desired.as_deref())?;
            // Keep a dead pane visible briefly instead of dropping to a
            // bare shell that would accept input meant for the TUI.
            let command = format!(
                "env {} {argv}; printf '\\n{}\\n'; sleep 3",
                super::cloud_secret_env_prefix(),
                self.profile.exit_banner()
            );
            // Pane env identifies the agent to `cadence self` and carries
            // the daemon's tracker and profile; -e args are tmux options,
            // never shell-interpreted.
            let pane_env = pane_env(&session, &self.state_dir, &self.context_env);
            let mut args: Vec<&str> = vec![
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
            ];
            for var in &pane_env {
                args.extend(["-e", var.as_str()]);
            }
            args.push(&command);
            self.tmux_ok(&args)?;
            let pane_pid = self.pane_pid(&session)?;
            // Bound the wait for the TUI to acquire its native session.
            match self.wait_owned_session(&session, pane_pid) {
                // The TUI exited on the chat it was told to resume —
                // the stored id is dead; report it so the next open
                // mints fresh. A freshly-minted id was never resumed,
                // so mint failures never trigger the clear.
                Err(e) => {
                    let cleared = if resuming {
                        resume_failed(&desired, &e)
                    } else {
                        None
                    };
                    return Err(cleared_err(e, cleared));
                }
                // The pane stayed up but never acquired the chat —
                // the resume never materialized, the chat is gone.
                Ok(None) => {
                    let e = Error::provider(format!(
                        "timed out waiting for the {} TUI to acquire its session",
                        self.profile.name()
                    ));
                    let cleared = if resuming {
                        resume_failed(&desired, &e)
                    } else {
                        None
                    };
                    return Err(cleared_err(e, cleared));
                }
                Ok(Some(found)) => {
                    // A changed owner fails closed — never clears,
                    // never adopts.
                    let native = self
                        .profile
                        .resolve_session(desired.as_deref(), Some(found))?;
                    (native, pane_pid, "respawned")
                }
            }
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

        self.record_pane_root(pane_pid, &generation);
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
            effort: None,
            pid: pane_pid,
            endpoint: Some(endpoint),
            generation: Some(generation),
            attach: Some(attach),
        })
    }

    /// Adopt a turn that survived a provably clean daemon stop (CAD-89):
    /// the pane must still be the recorded one — alive, same pid,
    /// holding the same native-session lock. Unlike the plain reattach
    /// there is no wait: the pane has been running all along, so a
    /// missing proof means the endpoint genuinely changed, not that it
    /// is still coming up. The recorded generation is reused, which is
    /// what keeps the in-flight token valid for `message_report`.
    fn open_adopted(
        &self,
        _agent: &Agent,
        adoption: &crate::store::AdoptEntry,
    ) -> Result<Identity> {
        let session = self.session();
        if !self.has_session(&session) {
            return Err(Error::provider("pane is gone"));
        }
        if self.pane_value(&session, "#{pane_dead}")? == "1" {
            return Err(Error::provider("pane process has exited"));
        }
        let pane_pid = self.pane_pid(&session)?;
        if pane_pid != adoption.pane_pid {
            return Err(Error::provider(format!(
                "pane pid changed (recorded {}, now {pane_pid})",
                adoption.pane_pid
            )));
        }
        self.profile
            .verify_ownership(&adoption.native_session, pane_pid)?;
        self.record_pane_root(pane_pid, &adoption.generation);

        let endpoint = format!("tmux://{}/{session}", self.socket);
        {
            let mut s = self.state.lock().unwrap();
            s.native_session = adoption.native_session.clone();
            s.pane_pid = pane_pid;
            s.generation = adoption.generation.clone();
            s.claims.clear(); // claims never survive a daemon restart
        }
        Ok(Identity {
            thread_id: adoption.native_session.clone(),
            session_id: adoption.native_session.clone(),
            model: None,
            effort: None,
            pid: pane_pid,
            endpoint: Some(endpoint),
            generation: Some(adoption.generation.clone()),
            attach: Some("adopted"),
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
        if prompt.is_empty() || prompt.len() > MAX_BODY {
            return Err(Error::pre_write("PTY messages must be 1–4000 characters"));
        }
        if has_control_chars(prompt) {
            return Err(Error::pre_write(
                "PTY messages must be a single line without control characters",
            ));
        }
        // Forbidden input prefixes: a leading character the TUI treats
        // as a command or mode switch (its own menu, a shell escape)
        // makes a verbatim paste an injection path — reject before any
        // byte reaches the pane and before the gate consumes a claim.
        if let Some(prefix) = prompt.trim_start().chars().next() {
            if self.profile.forbidden_prefixes().contains(&prefix) {
                return Err(Error::pre_write(format!(
                    "message body starts with '{prefix}', which {} treats \
                     as a command or mode switch — refusing to paste it \
                     into the terminal",
                    self.profile.name()
                )));
            }
        }
        // The gate's probe and the paste it admits are one critical
        // section: a concurrent `agent answer` (or second send) must
        // not interleave keys between the probe and the paste.
        let _paste_guard = self.paste_lock.lock().unwrap();
        self.check_gate(client_message_id)?;

        let token = {
            let s = self.state.lock().unwrap();
            registry::PTY_TURN_TOKENS.mint(&s.generation)
        };
        let session = self.session();

        // The render check is *differential*: "this paste added text",
        // not "the text is somewhere on screen". Capture before the
        // paste so an identical earlier body (a repeated routed
        // notification, a re-sent task) cannot pass for this one.
        let before = self.capture_visible()?;
        // The probe slice is the body's normalized tail — the end is
        // what stays visible on a horizontally-scrolled input line and
        // what a wrapped transcript renders last. Whitespace and inline
        // markdown markers are stripped on both sides so TUI line
        // wrapping/indentation and a markdown-rendered transcript echo
        // cannot hide a match. For routed `worker_result` bodies the tail covers the
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
        // Enter committed the turn — the pane state can no longer be
        // raced by an `agent answer`, so the lock can go.
        drop(_paste_guard);

        // Post-paste verification, bounded by RENDER_DEADLINE: the
        // slice's occurrence count must increase AND the input line must
        // be empty again — text rendered but still sitting in the input
        // means Enter never submitted (a staged draft is not a turn).
        // A miss inside the bound is evidence of a dropped paste, never
        // proof; the daemon decides per message kind what a miss means.
        let render_started = Instant::now();
        let mut render_decision = RenderDecision::new(RENDER_DEADLINE);
        loop {
            let styled = self.capture_visible_styled()?;
            let screen = sgr::strip(&styled);
            let observation = if normalize_screen(&screen).matches(&slice).count() > before_count {
                let cursor = self.cursor_pos(&session);
                RenderObservation::Visible {
                    input_nonempty: self.profile.analyze_styled(&styled, cursor).input_nonempty,
                }
            } else {
                RenderObservation::NotVisible
            };
            match render_decision.observe(render_started.elapsed(), observation) {
                Some(RenderOutcome::Submitted) => break,
                Some(outcome @ (RenderOutcome::Staged | RenderOutcome::NotRendered)) => {
                    // The miss carries what the pane actually showed — the
                    // screen tail before the paste and after the deadline,
                    // plus the probe verdict that admitted the send — so a
                    // fence records evidence, not just a verdict.
                    let reason = match outcome {
                        RenderOutcome::Staged => {
                            "paste rendered in the input line but was never submitted — \
                             Enter not observed; the draft is left untouched"
                        }
                        RenderOutcome::NotRendered => {
                            "pasted text never rendered in the pane — the TUI dropped it"
                        }
                        RenderOutcome::Submitted => unreachable!(),
                    };
                    let claim_probe = self
                        .state
                        .lock()
                        .unwrap()
                        .gate_probe
                        .as_ref()
                        .map(Probe::to_json);
                    return Err(Error::not_rendered(crate::error::RenderMiss {
                        reason: reason.to_string(),
                        before_tail: screen_tail(&before, 12),
                        after_tail: screen_tail(&screen, 12),
                        claim_probe,
                    }));
                }
                None => std::thread::sleep(Duration::from_millis(150)),
            }
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
        Err(Error::rejected(self.profile.respond_rejection()))
    }

    fn interrupt(&self) {
        let session = self.session();
        let _ = self.tmux(&["send-keys", "-t", &session, "C-c"]);
    }

    fn disconnected(&self) -> bool {
        // Disconnect is destructive — the daemon fences the endpoint
        // and force-closes the pane — so a single evidence miss must
        // not carry it. Rather than sleep inside the call (which
        // stalls the actor loop under load), exactly one probe runs
        // per idle tick and consecutive misses are counted across
        // ticks: a dead pane fails every probe and is fenced after
        // DISCONNECT_MISS_BUDGET ticks, a transient miss clears on
        // the next tick. Per-tick cost of a negative: one /proc scan
        // plus two cheap tmux reads — well under a second.
        if self.pane_alive() {
            self.state.lock().unwrap().disconnected_misses = 0;
            return false;
        }
        let mut s = self.state.lock().unwrap();
        s.disconnected_misses += 1;
        s.disconnected_misses >= DISCONNECT_MISS_BUDGET
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

    fn claim_ready(&self, by: Option<String>, force: bool) -> Result<Probe> {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("cannot claim readiness: pane is gone"));
        }
        self.verify_ownership(&session, &native)?;
        // The claim runs the same probe verified auto-ready uses: a
        // visibly busy pane refuses rather than letting a paste land
        // mid-turn. `--force` claims anyway — the busy verdict is
        // still recorded on the claim for the audit trail.
        let probe = self.probe()?;
        if !probe.idle && !force {
            return Err(Error::rejected(format!(
                "refusing readiness claim — {} \
                 (inspect with `agent capture`, or pass --force)",
                probe.reason
            )));
        }
        let mut state = self.state.lock().unwrap();
        if state.claims.len() >= CLAIM_CAPACITY {
            state.claims.pop_front();
        }
        state.claims.push_back(Claim {
            at: Instant::now(),
            by,
            probe: probe.clone(),
        });
        Ok(probe)
    }

    fn capture(&self) -> Result<String> {
        let session = self.session();
        self.tmux_ok(&["capture-pane", "-p", "-t", &session, "-S", "-120"])
    }

    fn probe(&self) -> Result<Probe> {
        let session = self.session();
        let cursor = self.cursor_pos(&session);
        Ok(self
            .profile
            .analyze_styled(&self.capture_visible_styled()?, cursor))
    }

    fn verify_owned_endpoint(
        &self,
        expected_pid: u32,
        expected_generation: &str,
        expected_native: Option<&str>,
    ) -> Result<()> {
        let (session, native, generation) = {
            let state = self.state.lock().unwrap();
            (
                state.session.clone(),
                state.native_session.clone(),
                state.generation.clone(),
            )
        };
        if generation != expected_generation {
            return Err(Error::rejected(
                "native endpoint generation changed while resolving memory identity",
            ));
        }
        if expected_native.is_some_and(|want| want != native) {
            return Err(Error::rejected(
                "native endpoint session changed while resolving memory identity",
            ));
        }
        if !self.has_session(&session) {
            return Err(Error::rejected("native endpoint session is no longer live"));
        }
        let pane_pid = self.pane_pid(&session)?;
        if pane_pid != expected_pid {
            return Err(Error::rejected(
                "native endpoint pane pid changed while resolving memory identity",
            ));
        }
        self.profile.verify_ownership(&native, pane_pid)
    }

    /// `agent answer`: the only input a menu accepts is its own choice
    /// key — never a paste. The fresh probe must still see the menu;
    /// anything else refuses so the keystroke cannot land in a prompt,
    /// a draft, or a running turn.
    fn answer_approval(&self, choice: &str) -> Result<Probe> {
        // Serialised against the send gate: the probe that verifies the
        // menu and the keys it admits are one critical section — a
        // paste or a second answer cannot interleave between them.
        let _paste_guard = self.paste_lock.lock().unwrap();
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("cannot answer: pane is gone"));
        }
        self.verify_ownership(&session, &native)?;
        let styled = self.capture_visible_styled()?;
        let screen = sgr::strip(&styled);
        let probe = self
            .profile
            .analyze_styled(&styled, self.cursor_pos(&session));
        if !probe.approval_menu {
            return Err(Error::rejected(format!(
                "refusing menu answer — the pane shows no approval menu \
                 ({}) (inspect with `agent capture`)",
                probe.reason
            )));
        }
        let keys = self.profile.approval_answer(&screen, choice)?;
        // `--` ends tmux option parsing so a key name can never be
        // read as a send-keys flag.
        let mut args = vec!["send-keys", "-t", session.as_str(), "--"];
        args.extend(keys.iter().map(String::as_str));
        self.tmux_ok(&args)?;
        Ok(probe)
    }

    fn sample_screen(&self) -> Result<(String, Probe)> {
        let session = self.session();
        let styled = self.capture_visible_styled()?;
        let probe = self
            .profile
            .analyze_styled(&styled, self.cursor_pos(&session));
        Ok((activity_hash(&sgr::strip(&styled)), probe))
    }

    fn update_params(&self, params: &Value) {
        self.auto_ready.store(
            params.get("auto_ready").and_then(|v| v.as_str()) == Some("verified"),
            AtomicOrdering::SeqCst,
        );
    }

    fn set_unclaimed_ok(&self, ok: bool) {
        self.unclaimed_ok.store(ok, AtomicOrdering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_screen, pane_env, tail_chars};

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

    /// CAD-282: Claude echoes the raw body in its input box but renders
    /// the submitted transcript copy as markdown, dropping backticks.
    /// Both echoes must match the same probe slice, or every body with
    /// inline code (the join bootstrap) reads as never submitted.
    #[test]
    fn markdown_rendered_echo_matches_raw_slice() {
        let body = "Run `cadence self` for this message's id and turn_id, then report: \
                    cadence message result <id> --token <turn_id> --text '<summary>'. \
                    List peers with `cadence agent list`.";
        let slice = normalize_screen(&tail_chars(body, super::PROBE_SLICE));
        // Input box before Enter: raw text, backticks intact.
        let input_box = format!("❯ {body}");
        // Transcript after Enter (Claude Code 2.1.280 capture, 120 cols):
        // wrapped, indented, inline code rendered without backticks.
        let transcript = "❯ Run cadence self for this message's id and turn_id, then report: cadence message \
                          result <id> --token\n  <turn_id> --text '<summary>'. List peers with cadence agent list.";
        assert_eq!(normalize_screen(&input_box).matches(&slice).count(), 1);
        assert_eq!(normalize_screen(transcript).matches(&slice).count(), 1);
        // Still differential: a screen without the body does not match.
        assert_eq!(
            normalize_screen("❯ \n  List peers with cadence")
                .matches(&slice)
                .count(),
            0
        );
    }

    /// CAD-310: the pane carries the agent's identity and then the
    /// daemon's tracker and profile.
    #[test]
    fn pane_env_carries_identity_then_daemon_context() {
        let context = [
            ("CADENCE_PM_DIR".to_string(), "/sbx/pm".to_string()),
            ("CADENCE_PROFILE".to_string(), "sandbox:x".to_string()),
        ];
        assert_eq!(
            pane_env("w1", std::path::Path::new("/sbx/state"), &context),
            [
                "CADENCE_ALIAS=w1",
                "CADENCE_STATE_DIR=/sbx/state",
                "CADENCE_PM_DIR=/sbx/pm",
                "CADENCE_PROFILE=sandbox:x",
            ]
        );
    }
}
