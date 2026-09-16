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
//! consumes an explicit operator readiness claim (`agent ready`, valid
//! [`READY_TTL`], single-use). The claim asserts what heuristics cannot
//! prove — idle input, no draft, no permission prompt on screen. Text is
//! delivered literally through a tmux buffer (`load-buffer` + bracketed
//! `paste-buffer -p` + `Enter`); no shell interpolation and no control
//! characters. At most one paste is attempted per claim.
//!
//! Terminal echo proves *submission*, never model receipt. A pasted
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
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::store::Agent;

use super::{AdapterHooks, Identity, ProviderAdapter, TurnResult};

/// How long an operator readiness claim stays valid for one send.
const READY_TTL: Duration = Duration::from_secs(60);
/// Bounded wait for the launched TUI to take a native session lock.
const OPEN_DEADLINE: Duration = Duration::from_secs(30);
/// Pause between bracketed paste and Enter so the TUI consumes it.
const PASTE_SETTLE: Duration = Duration::from_millis(300);
const BUFFER: &str = "cadence-msg";

struct PtyState {
    /// tmux session name (the agent alias).
    session: String,
    /// Native Devin session id (lock filename stem), learned at open.
    native_session: String,
    /// Pane pid verified against the session lock.
    pane_pid: u32,
    /// Endpoint generation minted per `open`; embedded in tokens.
    generation: String,
    /// Single-use operator readiness claim.
    claim: Option<Instant>,
}

pub struct DevinPtyAdapter {
    #[allow(dead_code)]
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
                claim: None,
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
    /// tmux mode, and still the lock owner — else the endpoint is dead;
    /// a fresh unconsumed operator claim is required — else requeue.
    fn check_gate(&self) -> Result<()> {
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
        let mut state = self.state.lock().unwrap();
        match state.claim {
            Some(at) if at.elapsed() <= READY_TTL => {
                state.claim = None; // consumed: at most one paste per claim
                Ok(())
            }
            _ => Err(Error::gate(
                "no fresh `agent ready` claim — an operator must verify the \
                 terminal is idle with an empty input before submission",
            )),
        }
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

        let endpoint = format!("tmux://{}/{session}", self.socket);
        {
            let mut s = self.state.lock().unwrap();
            s.native_session = native.clone();
            s.pane_pid = pane_pid;
            s.generation = generation.clone();
            s.claim = None; // a new endpoint can never inherit a claim
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
        _client_message_id: &str,
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
        self.check_gate()?;

        let token = {
            let s = self.state.lock().unwrap();
            format!("pty-{}-{}", s.generation, Uuid::new_v4().simple())
        };
        let session = self.session();

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

    fn claim_ready(&self) -> Result<()> {
        let (session, native) = self.session_and_native();
        if !self.has_session(&session) {
            return Err(Error::provider("cannot claim readiness: pane is gone"));
        }
        self.verify_ownership(&session, &native)?;
        self.state.lock().unwrap().claim = Some(Instant::now());
        Ok(())
    }

    fn capture(&self) -> Result<String> {
        let session = self.session();
        self.tmux_ok(&["capture-pane", "-p", "-t", &session, "-S", "-120"])
    }
}
