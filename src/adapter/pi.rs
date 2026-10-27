//! Managed Pi adapter over `pi --mode rpc` (`managed` endpoint,
//! provider `pi`) — CAD-322.
//!
//! Pi's RPC mode speaks newline-delimited JSON on stdio, but it is NOT
//! JSON-RPC: commands are `{"id":…,"type":"prompt",…}`, responses are
//! `{"id":…,"type":"response","command":…,"success":…}` and asynchronous
//! events carry only a top-level `type`. The transport therefore runs
//! in raw-lines mode (every line arrives as a notification whose method
//! is its `type`) and correlation is done here: a `response` line's
//! `id` resolves the matching request.
//!
//! Semantics mapped to Cadence:
//! - One durable message → one `prompt` command. Its `response`
//!   (`success:false` e.g. "No API key found") is the acceptance
//!   verdict; the turn's output streams afterwards and `agent_settled`
//!   — emitted unconditionally once no retry/compaction/queued
//!   continuation remains — ends the turn.
//! - `message_end` for an assistant message is the authoritative text;
//!   `stopReason` `aborted`/`error` maps to interrupted/failed.
//! - A master `400` `invalid_request` before any tool is the CAD-1076
//!   fresh-session continuation. Pi builds the provider HTTP body in
//!   its own process; this adapter only sends `prompt` and cannot
//!   measure that body or disable tools for a later prompt. A
//!   serialized-body refusal after a tool is therefore not continued:
//!   the turn fails with an explicit limitation and nothing is replayed.
//!   A token or context-window refusal is not that body refusal.
//! - `tool_execution_start`/`_end` → one `cadence/tool_use` /
//!   `cadence/tool_result` event each (redacted one-line summaries via
//!   `crate::store::tool_*_summary`, never raw I/O).
//! - `interrupt` is Pi's own `abort` command — never a signal first;
//!   SIGINT on the child's process group is only the fallback when
//!   stdin is already gone (same posture as the Claude adapter).
//! - `extension_ui_request`: fire-and-forget methods (`setStatus`,
//!   `notify`, …) are recorded only; dialog methods (`select`,
//!   `confirm`, `input`, `editor`) block Pi until answered, so slice 1
//!   auto-cancels them (`cancelled:true`) and records the refusal —
//!   brokered routing through `agent requests`/`respond` is follow-up
//!   work.
//! - Turn liveness is activity, not wall clock (same rule as Claude):
//!   any stdout event resets the clock; a turn fences `unknown` after
//!   `params.turn_idle_secs` of silence (default 900) or the optional
//!   `params.turn_max_secs` cap.
//! - Sessions differ by role. The MASTER is disposable by design (the
//!   brief): `--no-session`, so every `open` mints a fresh session id
//!   and the daemon rebuilds context from the continuity pack
//!   (CAD-324). A pi WORKER (CAD-544) instead keeps a persistent
//!   session file in its private dir under the state dir, so an idle
//!   stop + resume — and every later dispatch — continues the same Pi
//!   conversation; the daemon reads a stable `sessionId` off
//!   `get_state` as proof, and a lost file simply mints a new session
//!   plus a continuity pack (`Reason::New`).
//! - The master runs under the same Landlock confinement as Claude
//!   (`cadence confine`) but with Pi's own path set — `master/pi`, never
//!   `master/claude` or its `.credentials.json` — its own
//!   `PI_CODING_AGENT_DIR` (never the operator's `~/.pi`), an
//!   allowlisted child environment (the whole inherited env is dropped;
//!   only named non-credential variables are re-read), and a generated
//!   guard extension (`--no-extensions -e <guard>`) that blocks every
//!   tool except the master's allowlisted `cadence` Bash commands. The
//!   guard is a grammar — tokenize, charset-refuse every shell
//!   metacharacter, require `argv[0] == "cadence"`, then match an
//!   argv-prefix table — never a string prefix match into `bash -c`.
//! - A pi worker can opt into the same `cadence confine` launcher
//!   (CAD-556 — `join … pi --confine` or `pm.yaml [host]
//!   confine_pi_workers`, off by default): a per-worker Landlock
//!   policy grants its worktree, the repo's shared git dir, the
//!   effective/shared target dirs, the declared cargo + sccache
//!   caches, the PM tracker and its own dir under the state dir —
//!   reads the toolchain and system trees, and denies every `$HOME`
//!   secret (`~/.ssh` keys, `~/.gitconfig`, `~/.pi`, `~/.claude`,
//!   cargo `credentials.toml`, `~/.local/share` itself, and Devin
//!   credentials) — every other agent's dir and the daemon store
//!   by omission. `TMPDIR` is redirected into the worker dir (a shared
//!   `/tmp` grant would expose every lane, and a denied one makes
//!   rustc retry for ~1s) and `GIT_CONFIG_GLOBAL` at the worker's own
//!   seeded file (git fatals on an unreadable `~/.gitconfig`). The
//!   policy is a filesystem boundary only — Landlock does not gate
//!   networking (the provider API and git remotes stay reachable) or
//!   path-named unix-socket connects (the daemon socket is reachable
//!   by design); it is not a capability sandbox against a process with
//!   arbitrary exec. `agent show` reports `confined` and the emitted
//!   read/write sets.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use super::link::Incoming;
use super::registry;
use super::stdio::{EnvScrub, StdioAdapter};
use super::{AdapterHooks, Identity, ProviderAdapter, ProviderEnv, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

/// The literal a guarded Pi submission prepends to the task prompt. The
/// daemon reserves these bytes in its combined payload cap even when the guard
/// is currently off: readiness can change between planning and the turn, so
/// the reservation is conservative, not conditional.
pub(crate) const TASK_INPUT_PREFIX: &str = "Cadence task input (not a runtime command):\n";

/// Default inactivity window before a turn is `unknown`
/// (`params.turn_idle_secs`); `params.turn_max_secs` is the optional
/// absolute cap. Same rule as the Claude adapter.
const DEFAULT_TURN_IDLE: Duration = Duration::from_secs(900);
/// After `abort` Pi emits the aborted turn tail then `agent_settled` —
/// a bounded grace keeps a hung interrupt from parking the actor.
const INTERRUPT_GRACE: Duration = Duration::from_secs(60);
/// Bounded wait for a command's `response` frame.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// `set_thinking_level` levels live in the registry
// (`registry::PI_EFFORTS`, shared with launch-param validation).

/// Extension-UI methods that block Pi until an `extension_ui_response`
/// arrives. Slice 1 cancels them; everything else is recorded only.
const UI_DIALOG_METHODS: &[&str] = &["select", "confirm", "input", "editor"];

#[path = "pi_devin_catalog.rs"]
mod devin_catalog;

#[path = "pi_agenticos.rs"]
mod agenticos;

#[path = "pi_confine.rs"]
mod pi_confine;
#[path = "pi_guard.rs"]
mod pi_guard;
#[path = "pi_launch.rs"]
mod pi_launch;

use pi_confine::*;
use pi_guard::*;
use pi_launch::*;

pub use pi_confine::{
    launch_workdir, pi_master_confinement, pi_worker_confinement, refuse_confined_devin,
    refuse_confined_devin_agent, worker_confined, CONFINE_WORKER_EXTRA_READ_ENV,
    CONFINE_WORKER_EXTRA_WRITE_ENV,
};
pub use pi_launch::{copy_pi_auth, pi_agent_home, pi_config_dir};
pub(crate) use pi_launch::{pi_master_env_scrub, pi_worker_env_scrub, resolve_pi_launch_command};

/// Pi's display form of a provider HTTP error (`formatProviderError`):
/// `400 {…}` or `400: {…}`. Anything that is not that JSON body is
/// absent — a rate limit or a bare status is not a history refusal.
fn provider_error_body(error: &str) -> Option<Value> {
    let rest = error.trim_start().strip_prefix("400")?;
    let rest = rest.strip_prefix(':').unwrap_or(rest).trim();
    serde_json::from_str(rest).ok()
}

/// CAD-1076: the provider refused the request itself — HTTP 400 whose
/// JSON body carries `code: "invalid_request"`. A rate limit, an
/// outage, a credential error or any other code never matches.
/// [`serialized_body_rejected`] is the narrower serialized-body case;
/// it does not invent a byte cap the provider did not state.
fn history_rejected(body: &Value) -> bool {
    body.get("code")
        .or_else(|| body.pointer("/error/code"))
        .and_then(Value::as_str)
        == Some("invalid_request")
}

/// The provider refused the serialized HTTP body, as opposed to a token
/// or context-window limit. The provider's own message is the evidence;
/// this does not apply a locally chosen byte cap.
fn serialized_body_rejected(body: &Value) -> bool {
    if !history_rejected(body) {
        return false;
    }
    let Some(message) = body
        .get("message")
        .or_else(|| body.pointer("/error/message"))
        .and_then(Value::as_str)
    else {
        return false;
    };
    let lower = message.to_ascii_lowercase();
    !lower.contains("token")
        && !lower.contains("context window")
        && !lower.contains("context length")
        && !lower.contains("context_length")
        && lower.contains("request body")
        && (lower.contains("kib") || lower.contains("byte"))
}

/// Outcome of one settled Pi turn. `reset_session` is the only supported
/// continuation: a master history refusal before any tool, which the
/// daemon answers with one fresh session and the same message. After a
/// tool, `prompt` can still call tools, and Pi exposes no command that
/// continues from retained evidence with tools disabled. The turn stays
/// failed with [`serialized_body_limitation`] and is not submitted again.
struct PiSettlement {
    status: &'static str,
    error: Option<String>,
    reset_session: bool,
    limited: bool,
}

fn settle_pi_turn(acc: &mut TurnAcc, master: bool, interrupted_here: bool) -> PiSettlement {
    let aborted = acc.stop.as_deref() == Some("aborted");
    let (status, mut error) = if aborted || (interrupted_here && acc.error.is_some()) {
        ("interrupted", None)
    } else if let Some(err) = acc.error.take() {
        ("failed", Some(err))
    } else if acc.stop.as_deref() == Some("error") {
        (
            "failed",
            Some("pi turn ended with stopReason 'error'".to_string()),
        )
    } else {
        ("completed", None)
    };
    let body = error.as_deref().and_then(provider_error_body);
    // The no-tools guard is the side-effect boundary. Do not drop it
    // for a serialized-body refusal: a fresh prompt would replay
    // work that already ran.
    let reset_session =
        status == "failed" && !acc.tools && master && body.as_ref().is_some_and(history_rejected);
    let limited = match error.take() {
        Some(original)
            if status == "failed"
                && acc.tools
                && body.as_ref().is_some_and(serialized_body_rejected) =>
        {
            error = Some(serialized_body_limitation(&original));
            true
        }
        other => {
            error = other;
            false
        }
    };
    PiSettlement {
        status,
        error,
        reset_session,
        limited,
    }
}

fn serialized_body_limitation(original: &str) -> String {
    format!(
        "The provider rejected the serialized request body. This is not a token \
         or context-window limit. A tool already ran, so this turn was not \
         submitted again and its effects were not replayed. The original user \
         message, recorded tool evidence, and actor permissions are unchanged. \
         Continuation is not available: a new prompt can still call tools, and \
         Pi has no command that continues from retained evidence without \
         replaying them. Provider error: {original}"
    )
}

/// One in-flight command's response slot.
struct Pending {
    map: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    next: AtomicU64,
}

impl Pending {
    fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }
    /// Resolve the waiter for this `response` frame's `id`.
    fn resolve(&self, frame: &Value) {
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            return;
        };
        if let Some(tx) = self.map.lock().unwrap().remove(id) {
            let _ = tx.send(frame.clone());
        }
    }
    fn fail_all(&self, reason: &str) {
        for (_, tx) in self.map.lock().unwrap().drain() {
            let _ = tx.send(json!({"success": false, "error": reason}));
        }
    }
}

/// Per-turn accumulation, reset on each `prompt` and finalized into a
/// [`TurnOutcome`] by `agent_settled`.
#[derive(Default)]
struct TurnAcc {
    /// Authoritative assistant text from `message_end` — the LAST
    /// assistant message of the run is the report (a retry replaces it).
    text: Option<String>,
    /// Assistant `stopReason` of that message.
    stop: Option<String>,
    /// Provider-side error seen mid-turn (`message_end` error,
    /// `auto_retry_end` final failure, `extension_error`).
    error: Option<String>,
    /// A tool started in this run — its effects already happened, so
    /// the turn is never offered for a replay (CAD-1076).
    tools: bool,
}

struct Shared {
    hooks: AdapterHooks,
    pending: Pending,
    /// Finalized turns not yet consumed by a `run_turn` waiter.
    outcomes: Mutex<VecDeque<TurnAcc>>,
    outcome_cv: Condvar,
    /// The accumulating turn (events between `prompt` and
    /// `agent_settled`).
    turn: Mutex<TurnAcc>,
    /// Minted per `open`; prefixes this endpoint generation's turn ids.
    generation: Mutex<String>,
    /// CAD-575: the role the LAST `open` ran under — `session_set_model`
    /// gates on it (`master_allow` vs `worker_allow`/`allow`). False
    /// until an open lands, which is also "worker" — a session command
    /// without a live session fails at the transport first.
    master: AtomicBool,
    /// Set by `interrupt()` — the next settle's grace deadline.
    interrupt_at: Mutex<Option<Instant>>,
    /// The turn token `run_turn` is waiting on.
    active_turn: Mutex<Option<String>>,
    /// Last provider stdout event — the turn liveness clock.
    last_activity: Mutex<Instant>,
    idle_window: Mutex<Duration>,
    max_turn: Mutex<Option<Duration>>,
    /// Write-back channel for auto-cancelling extension UI dialogs —
    /// populated at `open` once the transport exists.
    transport: Mutex<Option<Arc<StdioAdapter>>>,
    dead: AtomicBool,
    guard_ready: AtomicBool,
    guard_receipts: Mutex<HashMap<String, GuardReceipt>>,
    /// CAD-1076: the last master turn the provider refused before any
    /// work — the one turn [`ProviderAdapter::reset_rejected_session`]
    /// may reset for. A serialized-body refusal after a tool is never
    /// stored here.
    rejected_turn: Mutex<Option<String>>,
    /// Native Pi `images` for the next prompt, then cleared.
    turn_images: Mutex<Vec<Value>>,
}

struct GuardReceipt {
    operation: String,
    turn_id: String,
    data: Option<Value>,
}

pub struct PiAdapter {
    /// Swapped at `open()` — the real command line carries flags that
    /// only exist once the agent row is read.
    transport: RwLock<Arc<StdioAdapter>>,
    shared: Arc<Shared>,
    log_path: PathBuf,
    state_dir: PathBuf,
    env: ProviderEnv,
    /// The split-mode guest uid (`agent_uid`) when the daemon runs split —
    /// `Some` selects the protected launch seam (`pi_guest`), which fails
    /// closed until the external prerequisites are satisfied (CAD-1012).
    agent_uid: Option<u32>,
    /// CAD-1098: the tool profile the next `open` launches with.
    profile: Mutex<crate::master::Profile>,
    /// CAD-1085: the closed caller identity of the attached owned console.
    protected_caller: Mutex<Option<Arc<crate::protected_runtime::RuntimeCaller>>>,
    /// CAD-1085: the stable agent uid whose owned runtime is attached.
    protected_agent_uid: Mutex<Option<String>>,
}

/// Identity evidence must include an actual namespace and model id;
/// a display name or a full requested id cannot replace that namespace.
fn pi_model_identity(state: &Value) -> Option<(&str, &str)> {
    let model = state.get("model")?;
    let provider = model.get("provider")?.as_str()?;
    let id = model.get("id")?.as_str()?;
    if provider.is_empty()
        || provider.contains('/')
        || provider.chars().any(char::is_whitespace)
        || id.trim().is_empty()
        || id.trim() != id
    {
        return None;
    }
    Some((provider, id))
}

fn resolved_pi_model(state: &Value) -> Option<(String, &str)> {
    let (provider, id) = pi_model_identity(state)?;
    Some((format!("{provider}/{id}"), id))
}

/// Pi's live model object is the modality contract (`input` contains
/// `"image"`). A missing list is not vision, and the model id is not a
/// substitute for that list.
fn selected_model_admits_image(state: &Value) -> Result<()> {
    let Some((provider, id)) = pi_model_identity(state) else {
        return Err(Error::pre_write(
            "Pi did not identify its selected model, so the attached library image was not sent. The model was left unchanged.",
        ));
    };
    let model = &state["model"];
    let Some(input) = model.get("input").and_then(Value::as_array) else {
        return Err(Error::pre_write(
            "Pi did not report input modalities for the selected model, so the attached library image was not sent. The model was left unchanged.",
        ));
    };
    if input.iter().any(|item| item.as_str() == Some("image")) {
        return Ok(());
    }
    Err(Error::pre_write(format!(
        "selected model {provider}/{id} admits no image input, so the attached library image was not sent. Choose a model whose reported input modalities include image; this model was left unchanged."
    )))
}

impl PiAdapter {
    pub fn new(
        hooks: AdapterHooks,
        log_path: &Path,
        env: &ProviderEnv,
        agent_uid: Option<u32>,
    ) -> Self {
        let shared = Arc::new(Shared {
            hooks,
            pending: Pending::new(),
            outcomes: Mutex::new(VecDeque::new()),
            outcome_cv: Condvar::new(),
            turn: Mutex::new(TurnAcc::default()),
            generation: Mutex::new(String::new()),
            master: AtomicBool::new(false),
            interrupt_at: Mutex::new(None),
            active_turn: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            idle_window: Mutex::new(DEFAULT_TURN_IDLE),
            max_turn: Mutex::new(None),
            transport: Mutex::new(None),
            dead: AtomicBool::new(false),
            guard_ready: AtomicBool::new(false),
            guard_receipts: Mutex::new(HashMap::new()),
            rejected_turn: Mutex::new(None),
            turn_images: Mutex::new(Vec::new()),
        });
        let routed = Arc::clone(&shared);
        let disconnected = Arc::clone(&shared);
        Self {
            // Bound to the empty generation — replaced by `open`'s mint.
            transport: RwLock::new(StdioAdapter::new_lines(
                &pi_command(env),
                // The placeholder transport is rebound per open —
                // either scrub is fine here.
                pi_master_env_scrub(),
                Box::new(move |incoming| routed.dispatch_for("", incoming)),
                Box::new(move || disconnected.disconnect_for("")),
            )),
            shared,
            log_path: log_path.to_path_buf(),
            env: env.clone(),
            state_dir: log_path
                .parent()
                .and_then(|p| p.parent())
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            agent_uid,
            profile: Mutex::new(crate::master::Profile::Home),
            protected_caller: Mutex::new(None),
            protected_agent_uid: Mutex::new(None),
        }
    }

    /// The emitted confinement, appended to the provider log so an
    /// operator can see why a path is unreadable (same as Claude).
    fn log_confinement(&self, role: &str, policy: &crate::confine::Policy) -> Result<()> {
        if let Some(dir) = self.log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut log = super::open_provider_log(&self.log_path)?;
        std::io::Write::write_all(
            &mut log,
            format!(
                "cadence: {role} confinement: {}\n",
                policy.to_args().join(" ")
            )
            .as_bytes(),
        )?;
        Ok(())
    }

    /// A transport bound to THIS open's generation: the reader thread
    /// of a previous endpoint can outlive `close()` by a few
    /// milliseconds, and its trailing events/EOF must never mark a
    /// reopened session dead or corrupt its accumulating turn (CAD-322
    /// round 2, I4).
    fn transport_for(
        &self,
        command: &[String],
        master: bool,
        generation: &str,
    ) -> Arc<StdioAdapter> {
        let routed = Arc::clone(&self.shared);
        let disconnected = Arc::clone(&self.shared);
        let (gen_dispatch, gen_disconnect) = (generation.to_string(), generation.to_string());
        let scrub = if master {
            pi_master_env_scrub()
        } else {
            pi_worker_env_scrub()
        };
        StdioAdapter::new_lines(
            command,
            scrub,
            Box::new(move |incoming| routed.dispatch_for(&gen_dispatch, incoming)),
            Box::new(move || disconnected.disconnect_for(&gen_disconnect)),
        )
    }

    /// A command response says only that Pi handled a slash command.
    /// The correlated runtime custom entry is the admission proof.
    fn guard_command(
        &self,
        operation: &str,
        turn_id: &str,
        request: &str,
        text: Option<&str>,
    ) -> Result<Value> {
        {
            let mut receipts = self.shared.guard_receipts.lock().unwrap();
            if receipts.contains_key(request) {
                return Err(Error::rejected("turn input request is already in progress"));
            }
            receipts.insert(
                request.to_string(),
                GuardReceipt {
                    operation: operation.to_string(),
                    turn_id: turn_id.to_string(),
                    data: None,
                },
            );
        }
        let mut envelope = json!({"request": request, "turn": turn_id});
        if let Some(text) = text {
            envelope["text"] = json!(text);
        }
        let response = self.request_with_timeout(
            "prompt",
            json!({
                "message": format!("/cadence-{operation}-turn {envelope}"),
            }),
            Duration::from_secs(5),
        );
        let proof = self
            .shared
            .guard_receipts
            .lock()
            .unwrap()
            .remove(request)
            .and_then(|r| r.data);
        if let Some(proof) = proof {
            return Ok(proof);
        }
        Err(Error::unknown(format!(
            "No correlated Pi runtime {operation} receipt; input acceptance is unknown ({})",
            response
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "command response is not admission proof".to_string())
        )))
    }

    /// A `request` answered `success:true` → its `data`, else the
    /// provider's own error via [`command_error`].
    fn checked(&self, command: &str, fields: Value) -> Result<Value> {
        let resp = self.request(command, fields)?;
        if resp.get("success").and_then(Value::as_bool) == Some(true) {
            Ok(resp.get("data").cloned().unwrap_or(Value::Null))
        } else {
            Err(self.command_error(command, &resp))
        }
    }

    fn command_error(&self, command: &str, resp: &Value) -> Error {
        let error = command_error(command, resp);
        if !self.shared.master.load(Ordering::SeqCst) {
            return error;
        }
        let config = pi_config_dir(&self.state_dir);
        let catalog = config.join("models.json");
        if !catalog.is_file() {
            let raw = resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Error::provider(format!(
                "Pi '{command}' refused. \
                 Pi master reads PI_CODING_AGENT_DIR={} — models.json is absent; \
                 configure this directory if the model requires a custom catalog, \
                 or check its authentication (provider said: {raw})",
                config.display()
            ));
        }
        Error::provider(format!(
            "{error}; Pi master reads PI_CODING_AGENT_DIR={} \
             (models.json is present; check the selected model and its authentication)",
            config.display()
        ))
    }

    /// `/model <id>` (CAD-551): `arg` is `provider/id`, a bare model id,
    /// or a name on the provider's own list. The answer reports the
    /// model that was running plus the one `set_model` returned — the
    /// reply is self-verifying, no extra `get_state`.
    fn session_set_model(&self, arg: &str) -> Result<Value> {
        let before = self.checked("get_state", json!({})).ok().and_then(|s| {
            s.pointer("/model/id")
                .or_else(|| s.pointer("/model/name"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let (provider, model_id) = match arg.split_once('/') {
            Some((p, id)) if !p.is_empty() && !id.is_empty() => (p.to_string(), id.to_string()),
            _ => {
                let listed = self.checked("get_available_models", json!({}))?;
                let models = listed.get("models").and_then(Value::as_array);
                let found = models.and_then(|ms| {
                    ms.iter().find(|m| {
                        m.get("id").and_then(Value::as_str) == Some(arg)
                            || m.get("name").and_then(Value::as_str) == Some(arg)
                    })
                });
                match found {
                    Some(m) => (
                        m.get("provider")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        m.get("id")
                            .and_then(Value::as_str)
                            .unwrap_or(arg)
                            .to_string(),
                    ),
                    None => {
                        return Err(Error::rejected(format!(
                            "pi lists no model '{arg}' — `/models` names what it offers"
                        )))
                    }
                }
            }
        };
        // CAD-559: a `/model` switch is a launch-equivalent decision —
        // the operator's allowlist gate runs before `set_model` crosses
        // the wire, exactly like the open path's re-check — under the
        // role this adapter opened as (CAD-575).
        let want = format!("{provider}/{model_id}");
        let pi_policy = settings_policy(&self.env)?;
        let role = if self.shared.master.load(Ordering::SeqCst) {
            "master"
        } else {
            "worker"
        };
        crate::pi_policy::require_allowed(pi_policy.as_ref(), role, &want)?;
        // Pi has already loaded its catalog and auth. Reopening performs the
        // credentialless AgenticOS admission before either can be used.
        if role == "worker" && provider == "agenticos" {
            return Err(Error::rejected(
                "select AgenticOS for the next worker launch and reopen; live model switching is unavailable",
            ));
        }
        let model = match self.checked(
            "set_model",
            json!({"provider": provider, "modelId": model_id}),
        ) {
            Ok(model) => model,
            Err(error) => {
                // The provider may have changed state before failing.
                self.close();
                return Err(error);
            }
        };
        // The ack is a hint like `--model` — `get_state` is the proof;
        // a silent fallback fails the command (same rule as open).
        let state = match self.checked("get_state", json!({})) {
            Ok(state) => state,
            Err(error) => {
                self.close();
                return Err(error);
            }
        };
        let Some((resolved_model, _)) = resolved_pi_model(&state) else {
            self.close();
            return Err(Error::provider("pi did not report a nonempty provider and model id after set_model — transport closed (CAD-603)"));
        };
        if let Err(error) = crate::pi_policy::require_safe_transport(&resolved_model) {
            self.close();
            return Err(error);
        }
        if resolved_model != want {
            self.close();
            return Err(Error::provider(format!(
                "pi reports model {} but '{want}' was set — the provider \
                 silently fell back instead of honoring set_model (CAD-559)",
                resolved_model
            )));
        }
        Ok(json!({"was": before, "model": model, "requested": want}))
    }

    /// `/effort <level>` (CAD-551): set, then verify what stuck — Pi
    /// answers success even on a silent fallback to `off`, like `open`.
    fn session_set_effort(&self, level: &str) -> Result<Value> {
        self.checked("set_thinking_level", json!({"level": level}))?;
        let state = self.checked("get_state", json!({}))?;
        let applied = state.get("thinkingLevel").and_then(Value::as_str);
        match applied {
            Some(l) if l == level => Ok(json!({"level": l})),
            got => Err(Error::provider(format!(
                "pi refused effort '{level}' (thinking level stayed at {}) — \
                 supported levels: {}",
                got.unwrap_or("unknown"),
                registry::PI_EFFORTS.join(", ")
            ))),
        }
    }

    /// One `{"id":…,"type":<command>,…}` frame → its `response`.
    /// `success:false` answers with the provider's `error` text — a
    /// definitive reply, never `OutcomeUnknown`. A timeout is unknown:
    /// the command may have been delivered.
    fn request(&self, command: &str, fields: Value) -> Result<Value> {
        self.request_with_timeout(command, fields, REQUEST_TIMEOUT)
    }

    fn request_with_timeout(
        &self,
        command: &str,
        fields: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = format!(
            "c{}",
            self.shared.pending.next.fetch_add(1, Ordering::SeqCst)
        );
        let (tx, rx) = mpsc::channel();
        self.shared
            .pending
            .map
            .lock()
            .unwrap()
            .insert(id.clone(), tx);
        let mut frame = json!({"id": id, "type": command});
        if let Some(obj) = fields.as_object() {
            for (k, v) in obj {
                frame[k] = v.clone();
            }
        }
        let sent = self.transport.read().unwrap().send(frame);
        if let Err(e) = sent {
            self.shared.pending.map.lock().unwrap().remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(resp) => Ok(resp),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared.pending.map.lock().unwrap().remove(&id);
                Err(Error::unknown(format!(
                    "No response to '{command}' within {}s; provider state is unknown",
                    timeout.as_secs()
                )))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::unknown(
                "Pi response channel closed; provider state is unknown",
            )),
        }
    }
}

impl Shared {
    /// Is `generation` the live endpoint generation? Set once per
    /// `open`; a previous transport's reader keeps its own mint, so
    /// anything it delivers after a reopen is dropped here.
    fn is_current(&self, generation: &str) -> bool {
        self.generation.lock().unwrap().as_str() == generation
    }

    fn dispatch_for(&self, generation: &str, incoming: Incoming) {
        if self.is_current(generation) {
            self.dispatch(incoming);
        }
    }

    /// A transport's EOF counts only for the generation that launched
    /// it — a stale EOF must never mark a reopened session dead.
    fn disconnect_for(&self, generation: &str) {
        if self.is_current(generation) {
            self.on_disconnect();
        }
    }

    fn dispatch(&self, incoming: Incoming) {
        let Incoming::Notification { method, params } = incoming else {
            return;
        };
        // Any parsed line is proof of life.
        *self.last_activity.lock().unwrap() = Instant::now();
        if method == "response" {
            self.pending.resolve(&params);
            return;
        }
        match method.as_str() {
            "entry_appended" => {
                if params.pointer("/entry/customType").and_then(Value::as_str)
                    == Some("cadence-turn-input")
                {
                    if let Some(data) = params.pointer("/entry/data") {
                        if let Some(request) = data.get("request").and_then(Value::as_str) {
                            let mut receipts = self.guard_receipts.lock().unwrap();
                            if let Some(expected) = receipts.get_mut(request) {
                                if data.get("version").and_then(Value::as_u64) == Some(1)
                                    && data.get("operation").and_then(Value::as_str)
                                        == Some(expected.operation.as_str())
                                    && data.get("turn").and_then(Value::as_str)
                                        == Some(expected.turn_id.as_str())
                                {
                                    expected.data = Some(data.clone());
                                }
                            }
                        }
                    }
                }
            }
            "message_end" => self.on_message_end(&params),
            "tool_execution_start" => self.on_tool_start(&params),
            "tool_execution_end" => self.on_tool_end(&params),
            "tool_execution_update" => self.on_tool_progress(&params),
            "extension_ui_request" => self.on_ui_request(&params),
            "extension_error" => {
                let msg = params
                    .get("error")
                    .and_then(Value::as_str)
                    .or_else(|| params.get("errorMessage").and_then(Value::as_str))
                    .unwrap_or("extension error");
                self.emit("cadence/pi_extension_error", &json!({"error": msg}));
            }
            "auto_retry_start" => self.emit(
                "cadence/pi_retry",
                &json!({"attempt": params.get("attempt"),
                        "max_attempts": params.get("maxAttempts"),
                        "error": params.get("errorMessage")}),
            ),
            "auto_retry_end" if params.get("success") != Some(&Value::Bool(true)) => {
                let err = params
                    .get("finalError")
                    .and_then(Value::as_str)
                    .unwrap_or("pi auto-retry exhausted");
                self.turn.lock().unwrap().error = Some(err.to_string());
            }
            "agent_settled" => {
                let acc = std::mem::take(&mut *self.turn.lock().unwrap());
                self.outcomes.lock().unwrap().push_back(acc);
                self.outcome_cv.notify_all();
            }
            _ => {}
        }
    }

    /// `message_end.message` is authoritative: for an assistant message
    /// it holds the final content, `stopReason` and, on provider
    /// failure, `errorMessage`.
    fn on_message_end(&self, event: &Value) {
        let message = event.get("message").unwrap_or(&Value::Null);
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let text = message
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let mut acc = self.turn.lock().unwrap();
        if !text.trim().is_empty() {
            acc.text = Some(text);
        }
        acc.stop = message
            .get("stopReason")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(err) = message.get("errorMessage").and_then(Value::as_str) {
            acc.error = Some(err.to_string());
        }
    }

    fn on_tool_start(&self, event: &Value) {
        self.turn.lock().unwrap().tools = true;
        let name = event.get("toolName").and_then(Value::as_str).unwrap_or("?");
        let args = event.get("args").unwrap_or(&Value::Null);
        self.emit(
            "cadence/tool_use",
            &json!({
                "tool": name,
                "summary": crate::store::tool_summary(name, args),
                "tool_use_id": event.get("toolCallId"),
            }),
        );
    }

    /// `partialResult` is cumulative output so far — progress is recorded
    /// as a compact event, never the raw stream.
    fn on_tool_progress(&self, event: &Value) {
        self.emit(
            "cadence/tool_progress",
            &json!({
                "tool_use_id": event.get("toolCallId"),
                "tool": event.get("toolName"),
            }),
        );
    }

    fn on_tool_end(&self, event: &Value) {
        let result = event.get("result").unwrap_or(&Value::Null);
        let content = result.get("content").unwrap_or(&Value::Null);
        let summary = crate::store::tool_result_summary(content);
        let is_error = event
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // The master's guard blocks with its own reason text (`cadence
        // master …`) in the result content — a refusal, not a failure
        // (CAD-551). Ordinary `isError` output stays a failure.
        let refused = is_error && summary.starts_with("cadence master");
        self.emit(
            "cadence/tool_result",
            &json!({
                "summary": summary,
                "is_error": is_error,
                "refused": refused,
                "tool_use_id": event.get("toolCallId"),
            }),
        );
    }

    /// Fire-and-forget UI requests are recorded only. Dialog methods
    /// block Pi until answered; slice 1 has no broker route, so they
    /// are cancelled — recorded either way (never raw into the thread).
    fn on_ui_request(&self, event: &Value) {
        let ui_method = event.get("method").and_then(Value::as_str).unwrap_or("");
        let id = event.get("id").cloned();
        let dialog = UI_DIALOG_METHODS.contains(&ui_method);
        self.emit(
            "cadence/pi_ui_request",
            &json!({"ui_method": ui_method, "title": event.get("title"),
                    "cancelled": dialog}),
        );
        if dialog {
            if let (Some(id), Some(t)) = (id, self.transport.lock().unwrap().clone()) {
                let _ = t.try_send(json!({
                    "type": "extension_ui_response",
                    "id": id,
                    "cancelled": true,
                }));
            }
        }
    }

    fn emit(&self, method: &str, params: &Value) {
        (self.hooks.on_event)(method, params.clone());
    }

    /// Transport EOF: fail pending commands and wake turn waiters.
    fn on_disconnect(&self) {
        self.guard_ready.store(false, Ordering::SeqCst);
        self.dead.store(true, Ordering::SeqCst);
        self.pending.fail_all("Pi process disconnected");
        let _guard = self.outcomes.lock().unwrap();
        self.outcome_cv.notify_all();
    }
}

/// The provider's own error text for a failed command — credential
/// failures get the Needs-you remediation, never a raw provider dump
/// in the thread (the brief's F18 lesson).
fn command_error(command: &str, resp: &Value) -> Error {
    let raw = resp
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let credentials = raw.contains("API key")
        || raw.contains("api key")
        || raw.contains("/login")
        || raw.contains("auth")
        || raw.contains("credential");
    if credentials {
        Error::provider(format!(
            "Pi has no usable credentials for this model — sign the master in: \
             `PI_CODING_AGENT_DIR=<state>/master/pi pi` then `/login`, or \
             `cadence master start --provider pi --copy-login` (provider said: {raw})"
        ))
    } else {
        Error::provider(format!("pi '{command}' refused: {raw}"))
    }
}

impl ProviderAdapter for PiAdapter {
    fn set_session_profile(&self, profile: crate::master::Profile) {
        *self.profile.lock().unwrap() = profile;
    }

    fn session_profile(&self) -> crate::master::Profile {
        *self.profile.lock().unwrap()
    }

    fn note_activity(&self) {
        *self.shared.last_activity.lock().unwrap() = Instant::now();
    }

    fn activity_at(&self) -> Option<Instant> {
        Some(*self.shared.last_activity.lock().unwrap())
    }

    /// Every open is a fresh `pi --mode rpc --no-session` process:
    /// `get_state` yields its session id, which becomes the Cadence
    /// thread id — a new thread each (re)open, so the daemon rebuilds
    /// context from the continuity pack instead of resuming Pi state.
    /// `params.model` rides `--model` (Pi resolves `provider/id` and
    /// pattern syntax at startup); `params.effort` goes through
    /// `set_thinking_level` and is verified against `get_state`, since
    /// Pi silently falls back on an unsupported level.
    fn open(&self, agent: &Agent) -> Result<Identity> {
        refuse_confined_devin_agent(agent)?;
        // CAD-1085: when the reviewed root profile is active there is exactly
        // one native launch path — the owner's measured OCI console. A missing
        // coordinator/owner refuses; a host same-UID process is never a
        // fallback. The older split seam (`agent_uid`) keeps its own refusal
        // only for deployments without the reviewed profile.
        let protected_profile = crate::protected_runtime::protected_profile_active();
        #[cfg(not(all(unix, target_os = "linux")))]
        if self.agent_uid.is_some() || protected_profile {
            super::pi_guest::protected_prereqs_satisfied()?;
            return Err(Error::rejected("protected managed-Pi launch is Linux-only"));
        }
        #[cfg(all(unix, target_os = "linux"))]
        let protected = if protected_profile {
            None
        } else if let Some(uid) = self.agent_uid {
            let generation = Uuid::new_v4().simple().to_string();
            Some(super::pi_guest::GuestCtx::establish(
                agent,
                &self.env,
                uid,
                &generation,
            )?)
        } else {
            None
        }; // No caller flag/marker/command can construct this context.
        let master = crate::master::is_master(&agent.alias);
        let params = agent.params.clone().unwrap_or(Value::Null);
        // CAD-559: pi launches only on an explicit model the operator
        // allowlisted — `master start`/`register` decide it, and a
        // hand-edited or stale row is refused here rather than falling
        // back to whatever Pi would pick.
        let want = params
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if want.trim().is_empty() {
            return Err(Error::rejected(format!(
                "pi agent '{}' has no model configured — pass --model \
                 <provider/id> or configure the {} default in Settings > Agents; a \
                 silent provider fallback is never used (CAD-559)",
                agent.alias,
                if master { "master" } else { "worker" }
            )));
        }
        let pi_policy = settings_policy(&self.env)?;
        crate::pi_policy::require_allowed(
            pi_policy.as_ref(),
            if master { "master" } else { "worker" },
            want,
        )?;
        // CAD-1085: open the owned console before the transport exists so the
        // adapter's generation tag is the measured runtime's, and so a failed
        // bootstrap refuses before any prompt state is touched.
        #[cfg(all(unix, target_os = "linux"))]
        let mut protected_console = if protected_profile {
            let provider = crate::daemon::protected_runtime::console_provider()?;
            Some(provider.open_console(
                crate::daemon::protected_runtime::ConsoleSubject {
                    alias: &agent.alias,
                    agent_uid: &agent.uid,
                    created: agent.created,
                },
                &self.log_path,
            )?)
        } else {
            None
        };
        self.shared.master.store(master, Ordering::SeqCst);
        if master && self.agent_uid.is_none() && !protected_profile {
            let agenticos_reads = agenticos_read_extension(&self.env)?.is_some();
            write_pi_guard(
                &self.state_dir,
                agenticos_reads,
                *self.profile.lock().unwrap(),
            )?;
        }
        self.shared.guard_ready.store(false, Ordering::SeqCst);
        let guard_path = if self.agent_uid.is_none() && !protected_profile {
            Some(write_pi_turn_guard(&self.state_dir, &agent.alias)?.canonicalize()?)
        } else {
            None
        }; // Immutable protected guard/graph policy is not elected here.
        let (command, confinement) = if self.agent_uid.is_none() && !protected_profile {
            launch_command(&self.env, &self.state_dir, agent)?
        } else {
            (vec!["cadence-protected-fd-exec-only".into()], None)
        };
        if let Some(policy) = &confinement {
            let role = if master { "master" } else { "worker" };
            self.log_confinement(role, policy)?;
        }
        #[cfg(all(unix, target_os = "linux"))]
        let generation = if let Some(console) = protected_console.as_ref() {
            console.generation_tag.clone()
        } else {
            protected
                .as_ref()
                .map(|ctx| ctx.segments().generation_hex())
                .unwrap_or_else(|| Uuid::new_v4().simple().to_string()[..12].to_string())
        };
        #[cfg(not(all(unix, target_os = "linux")))]
        let generation = Uuid::new_v4().simple().to_string()[..12].to_string();
        *self.shared.generation.lock().unwrap() = generation.clone();
        self.shared.dead.store(false, Ordering::SeqCst);
        let mut env = if self.agent_uid.is_none() {
            let mut env = vec![
                ("CADENCE_ALIAS".to_string(), agent.alias.clone()),
                (
                    "CADENCE_STATE_DIR".to_string(),
                    self.state_dir.to_string_lossy().to_string(),
                ),
            ];
            env.extend(super::daemon_context_env(&self.env));
            if master {
                let pm = self
                    .env
                    .var("CADENCE_PM_DIR")
                    .filter(|v| !v.is_empty())
                    .is_none()
                    .then(crate::issue::default_dir)
                    .and_then(Result::ok);
                env.extend(crate::master::env_overrides(
                    "pi",
                    &self.state_dir,
                    pm.as_deref(),
                    master_confined(&self.env, agent),
                ));
                if *self.profile.lock().unwrap() == crate::master::Profile::App {
                    env.push((
                        crate::master::CONVERSATION_ENV.to_string(),
                        "app".to_string(),
                    ));
                }
            }
            // CAD-570: every pi agent gets its own XDG_CACHE_HOME inside
            // its private dir, created 0700 — pi-devin's model catalog
            // lives at `$XDG_CACHE_HOME/pi-devin/models.json`, and under
            // Landlock the operator's `~/.cache` is denied outright (the
            // smoke host's confined master EACCES'd it every open). The
            // dir is inside the already-granted provider/worker dir, so
            // confinement needs nothing new; unconfined, the explicit
            // value keeps a worker off the shared `~/.cache` its env
            // allowlist would otherwise inherit — two agents never share
            // one catalog file. Push order is deliberate: the worker's
            // env keep-list re-inherits the operator's XDG_CACHE_HOME and
            // `env_overrides` sets none, so this pair always wins.
            let cache_dir = pi_cache_dir(&self.state_dir, &agent.alias)?;
            ensure_private_dir(&cache_dir)?;
            env.push((
                "XDG_CACHE_HOME".to_string(),
                cache_dir.to_string_lossy().to_string(),
            ));
            if !master && want.starts_with("devin/") {
                let source_root = self
                    .env
                    .var("XDG_CACHE_HOME")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        self.env
                            .var("HOME")
                            .filter(|v| !v.is_empty())
                            .map(|v| PathBuf::from(v).join(".cache"))
                    })
                    .ok_or_else(|| {
                        Error::rejected("Offline Devin catalog source is unavailable")
                    })?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| Error::rejected("Offline Devin catalog clock is unavailable"))?
                    .as_millis();
                let now = u64::try_from(now)
                    .map_err(|_| Error::rejected("Offline Devin catalog clock is unavailable"))?;
                devin_catalog::seed(
                    &cache_dir,
                    &source_root.join("pi-devin/models.json"),
                    want,
                    now,
                )?;
            }
            if !master {
                // CAD-544 worker: a private `PI_CODING_AGENT_DIR` under the
                // state dir — auth arrives as the scoped file copy, never
                // via env or argv; `~/.pi` is never inherited. An
                // unattended worker must not stall on a git credential
                // prompt, so GIT_TERMINAL_PROMPT=0.
                let config = pi_worker_config_dir(&self.state_dir, &agent.alias)?;
                let operator = crate::master::operator_provider_config(
                    "pi",
                    self.env.var("PI_CODING_AGENT_DIR"),
                    self.env.var("HOME"),
                );
                if want.starts_with("agenticos/") {
                    agenticos::seed(&config, &pi_config_dir(&self.state_dir), want)?;
                } else if let Some(operator) = operator.as_deref() {
                    copy_pi_auth(&config, operator)?;
                } else {
                    ensure_private_dir(&config)?;
                }
                env.push((
                    "PI_CODING_AGENT_DIR".to_string(),
                    config.to_string_lossy().to_string(),
                ));
                env.push(("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()));
                if confinement.is_some() {
                    // CAD-556: under Landlock the daemon's TMPDIR may be a
                    // shared root — the worker gets its own so `mktemp`
                    // and friends stay inside the policy. And git reads
                    // `~/.gitconfig` fatally on EACCES: point the global
                    // config at the worker's file (seeded with the
                    // operator's identity — repo-local `user.*` still
                    // outranks it, empty is fine).
                    let dir = pi_worker_dir(&self.state_dir, &agent.alias)?;
                    let tmp = dir.join("tmp");
                    ensure_private_dir(&tmp)?;
                    seed_worker_gitconfig(&dir, &self.env)?;
                    env.push(("TMPDIR".to_string(), tmp.to_string_lossy().to_string()));
                    env.push((
                        "GIT_CONFIG_GLOBAL".to_string(),
                        dir.join("gitconfig").to_string_lossy().to_string(),
                    ));
                }
            }
            env
        } else {
            Vec::new()
        }; // Helper privately derives protected HOME/env/cwd.
           // No update checks or telemetry on any managed startup path.
        env.push(("PI_OFFLINE".to_string(), "1".to_string()));
        let idle_secs = params
            .get("turn_idle_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TURN_IDLE.as_secs());
        *self.shared.idle_window.lock().unwrap() = Duration::from_secs(idle_secs.max(1));
        *self.shared.max_turn.lock().unwrap() = params
            .get("turn_max_secs")
            .and_then(Value::as_u64)
            .map(|s| Duration::from_secs(s.max(1)));
        *self.shared.last_activity.lock().unwrap() = Instant::now();
        let transport = self.transport_for(&command, master, &generation);
        // The write-back channel is armed BEFORE launch — Pi can emit
        // a blocking extension_ui_request from its first line, and a
        // dispatch that found no transport would leave it unanswered.
        *self.shared.transport.lock().unwrap() = Some(Arc::clone(&transport));
        // Own the candidate before launch. Every initialization error,
        // including launch/effort setup, must revoke its prompt capability.
        // Keep all fallible post-launch work inside one cleanup boundary.
        *self.transport.write().unwrap() = Arc::clone(&transport);
        let workdir = launch_workdir(agent).to_string_lossy().into_owned();
        let initialized = (|| {
            #[cfg(all(unix, target_os = "linux"))]
            let pid = if let Some(console) = protected_console.take() {
                *self.protected_caller.lock().unwrap() = Some(Arc::new(console.caller));
                *self.protected_agent_uid.lock().unwrap() = Some(agent.uid.clone());
                transport.adopt_console(console.console)?
            } else {
                match protected {
                    Some(ctx) => {
                        let routing = crate::protected_pi_profile::Routing::for_agent(master, want)
                            .map_err(Error::rejected)?;
                        transport.launch_protected(ctx, &routing, &self.log_path)?
                    }
                    None => transport.launch(&workdir, &self.log_path, &env)?,
                }
            };
            #[cfg(not(all(unix, target_os = "linux")))]
            let pid = transport.launch(&workdir, &self.log_path, &env)?;
            // Effort: validate against the model's real levels, then verify
            // what stuck — Pi answers success even on a silent fallback.
            if let Some(effort) = params.get("effort").and_then(Value::as_str) {
                let applied = self
                    .request("set_thinking_level", json!({"level": effort}))
                    .and_then(|r| {
                        if r.get("success").and_then(Value::as_bool) == Some(true) {
                            Ok(())
                        } else {
                            Err(self.command_error("set_thinking_level", &r))
                        }
                    })
                    .and_then(|()| self.request("get_state", json!({})))
                    .map(|r| {
                        r.pointer("/data/thinkingLevel")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    });
                match applied {
                    Ok(Some(level)) if level == effort => {}
                    Ok(got) => {
                        return Err(Error::provider(format!(
                            "pi refused effort '{effort}' (thinking level stayed at \
                         {}) — supported levels: {}",
                            got.as_deref().unwrap_or("unknown"),
                            registry::PI_EFFORTS.join(", ")
                        )))
                    }
                    Err(e) => return Err(e),
                }
            }
            // An old/mocked runtime may not support the extension commands.
            // Ordinary queued turns remain compatible; native input is disabled.
            let guard_ready = guard_path.is_some()
                && self
                    .checked("get_commands", json!({}))
                    .ok()
                    .is_some_and(|data| {
                        data.get("commands")
                            .and_then(Value::as_array)
                            .is_some_and(|commands| {
                                [
                                    "cadence-bind-turn",
                                    "cadence-steer-turn",
                                    "cadence-abandon-turn",
                                ]
                                .iter()
                                .all(|name| {
                                    commands.iter().any(|command| {
                                        command.get("name").and_then(Value::as_str) == Some(*name)
                                            && command.get("source").and_then(Value::as_str)
                                                == Some("extension")
                                            && command
                                                .pointer("/sourceInfo/path")
                                                .and_then(Value::as_str)
                                                == guard_path
                                                    .as_ref()
                                                    .and_then(|path| path.to_str())
                                    })
                                })
                            })
                    });
            self.shared.guard_ready.store(guard_ready, Ordering::SeqCst);
            let state = self.request("get_state", json!({}))?;
            if state.get("success").and_then(Value::as_bool) != Some(true) {
                return Err(self.command_error("get_state", &state));
            }
            let data = state.get("data").cloned().unwrap_or(Value::Null);
            let session_id = data
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            // CAD-1752: the session file Pi actually opened (a lane
            // relaunch forks into a new one) is this worker's current one.
            if !master {
                if let Some(file) = data.get("sessionFile").and_then(Value::as_str) {
                    record_current_session(&self.state_dir, &agent.alias, file);
                }
            }
            let Some((reported_full, reported_id)) = resolved_pi_model(&data) else {
                return Err(Error::provider("pi did not report a nonempty provider and model id at open — transport closed (CAD-603)"));
            };
            // A bare worker id may resolve to Cursor. No conversation has
            // crossed RPC yet: close before a turn can launch its argv child.
            crate::pi_policy::require_safe_transport(&reported_full)?;
            // CAD-559: Pi answers get_state with whatever model it fell
            // back to — `--model` is a hint, not a contract. Refuse the
            // launch unless the running model is the allowlisted one.
            let matches = if want.contains('/') {
                reported_full == want
            } else {
                reported_id == want
            };
            if !matches {
                let config_note = if master {
                    let config = pi_config_dir(&self.state_dir);
                    format!(
                        "; Pi master reads PI_CODING_AGENT_DIR={} (models.json is {})",
                        config.display(),
                        if config.join("models.json").is_file() {
                            "present"
                        } else {
                            "absent"
                        }
                    )
                } else {
                    String::new()
                };
                return Err(Error::provider(format!(
                    "pi reports model {} but '{want}' was requested — the \
                 provider silently fell back instead of honoring \
                 --model (CAD-559){config_note}",
                    reported_full
                )));
            }
            let model = Some(reported_full);
            let effort = data
                .get("thinkingLevel")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(Identity {
                thread_id: session_id.clone(),
                session_id,
                model,
                effort,
                pid,
                endpoint: None,
                generation: Some(generation),
                attach: None,
            })
        })();
        if initialized.is_err() {
            self.close();
        }
        initialized
    }

    fn bind_turn_images(&self, images: &[super::TurnImage]) -> Result<()> {
        if images.is_empty() {
            self.shared.turn_images.lock().unwrap().clear();
            return Ok(());
        }
        let mut encoded = Vec::with_capacity(images.len());
        for image in images {
            if image.bytes.len() > 4 * 1024 * 1024
                || !matches!(image.media_type, "image/png" | "image/jpeg" | "image/webp")
            {
                return Err(Error::pre_write(
                    "retained image is outside the native prompt bound, so it was not sent",
                ));
            }
            encoded.push(json!({
                "type": "image",
                "mimeType": image.media_type,
                "data": base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &image.bytes,
                ),
            }));
        }
        *self.shared.turn_images.lock().unwrap() = encoded;
        Ok(())
    }

    /// One durable message → one `prompt` command → `agent_settled`.
    /// The response gates acceptance; the settle carries the outcome.
    fn run_turn(
        &self,
        prompt: &str,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        self.run_turn_slotted(prompt, None, client_message_id, on_started)
    }

    /// CAD-1009: the token is minted here, before the prompt is sent, so
    /// the scoped-turn line shows exactly the token `on_started` hands
    /// the daemon.
    fn run_turn_slotted(
        &self,
        prompt: &str,
        slot: Option<&str>,
        client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        if !self.shared.turn_images.lock().unwrap().is_empty() {
            let state = match self.checked("get_state", json!({})) {
                Ok(state) => state,
                Err(error) => {
                    self.shared.turn_images.lock().unwrap().clear();
                    return Err(error);
                }
            };
            if let Err(error) = selected_model_admits_image(&state) {
                self.shared.turn_images.lock().unwrap().clear();
                return Err(error);
            }
        }
        let generation = self.shared.generation.lock().unwrap().clone();
        let turn_id = registry::PI_MANAGED_TURN_TOKENS.mint(&generation);
        // CAD-1085 r7: bind the durable server-owned model task BEFORE any
        // prompt frame. No authority comes from prompt text or task args; the
        // guard is held for the turn and fences on drop/settle/interrupt.
        let _native_task = self.bind_native_task(client_message_id, &turn_id)?;
        let prompt = &super::with_turn_token(prompt, slot, &turn_id);
        *self.shared.interrupt_at.lock().unwrap() = None;
        // A settled-but-unconsumed turn is stale — it belongs to a
        // message the daemon already fenced.
        {
            let mut queue = self.shared.outcomes.lock().unwrap();
            while let Some(stale) = queue.pop_front() {
                self.shared
                    .emit("cadence/stale_result", &json!({"stop": stale.stop}));
            }
        }
        *self.shared.turn.lock().unwrap() = TurnAcc::default();
        let guarded = self.native_turn_steering();
        if guarded {
            let binding =
                match self.guard_command("bind", &turn_id, &format!("bind-{turn_id}"), None) {
                    Ok(binding) => binding,
                    Err(error) => {
                        self.shared.turn_images.lock().unwrap().clear();
                        return Err(error);
                    }
                };
            if binding.get("outcome").and_then(Value::as_str) != Some("bound") {
                self.shared.turn_images.lock().unwrap().clear();
                return Err(Error::rejected("Pi runtime refused the new turn binding"));
            }
        }
        let submitted = if guarded {
            // RPC does not expose expandPromptTemplates:false. Do not let a
            // plain task body invoke our private slash-control commands.
            format!("{TASK_INPUT_PREFIX}{prompt}")
        } else {
            prompt.to_string()
        };
        let master = self.shared.master.load(Ordering::SeqCst);
        let mut held = self.shared.turn_images.lock().unwrap();
        let payload = if held.is_empty() {
            json!({"message": submitted})
        } else if master {
            // A pre-tool history refusal retries this same prompt. Keep the
            // blocks for that one retry; the worker path cannot retry.
            json!({"message": submitted, "images": held.clone()})
        } else {
            let images = std::mem::take(&mut *held);
            json!({"message": submitted, "images": images})
        };
        drop(held);
        let resp = match self.request("prompt", payload) {
            Ok(resp) => resp,
            Err(error) => {
                self.shared.turn_images.lock().unwrap().clear();
                return Err(error);
            }
        };
        if resp.get("success").and_then(Value::as_bool) != Some(true) {
            self.shared.turn_images.lock().unwrap().clear();
            if guarded {
                // Definite prompt refusal can leave an unused pending binding.
                // Clear only that exact unused binding, never an active run.
                let _ =
                    self.guard_command("abandon", &turn_id, &format!("abandon-{turn_id}"), None);
            }
            return Err(self.command_error("prompt", &resp));
        }
        *self.shared.active_turn.lock().unwrap() = Some(turn_id.clone());
        on_started(&turn_id);
        let start = Instant::now();
        *self.shared.last_activity.lock().unwrap() = start;
        let idle_window = *self.shared.idle_window.lock().unwrap();
        let max_turn = *self.shared.max_turn.lock().unwrap();
        let waited = (|| -> Result<TurnAcc> {
            let mut queue = self.shared.outcomes.lock().unwrap();
            loop {
                if let Some(acc) = queue.pop_front() {
                    return Ok(acc);
                }
                if self.shared.dead.load(Ordering::SeqCst) {
                    return Err(Error::unknown(
                        "Pi process exited before the turn settled; outcome is unknown",
                    ));
                }
                let now = Instant::now();
                let last = *self.shared.last_activity.lock().unwrap();
                let idle_left = (last + idle_window).saturating_duration_since(now);
                if idle_left.is_zero() {
                    return Err(Error::unknown(format!(
                        "No provider event for {}s; outcome is unknown",
                        idle_window.as_secs()
                    )));
                }
                let max_left = max_turn.map(|cap| (start + cap).saturating_duration_since(now));
                if matches!(max_left, Some(d) if d.is_zero()) {
                    return Err(Error::unknown(format!(
                        "Turn exceeded turn_max_secs ({}s); provider outcome needs review",
                        max_turn.unwrap_or_default().as_secs()
                    )));
                }
                let interrupt_deadline = self
                    .shared
                    .interrupt_at
                    .lock()
                    .unwrap()
                    .map(|at| at + INTERRUPT_GRACE);
                if let Some(at) = interrupt_deadline {
                    if now >= at {
                        return Err(Error::unknown(
                            "No agent_settled after abort; provider outcome is unknown",
                        ));
                    }
                }
                let mut remaining = idle_left;
                if let Some(d) = max_left {
                    remaining = remaining.min(d);
                }
                if let Some(at) = interrupt_deadline {
                    remaining = remaining.min(at.saturating_duration_since(now));
                }
                let (guard, _) = self
                    .shared
                    .outcome_cv
                    .wait_timeout(queue, remaining.min(Duration::from_millis(250)))
                    .unwrap();
                queue = guard;
            }
        })();
        *self.shared.active_turn.lock().unwrap() = None;
        let mut acc = match waited {
            Ok(acc) => acc,
            Err(error) => {
                self.shared.turn_images.lock().unwrap().clear();
                return Err(error);
            }
        };
        if guarded {
            // The actor cannot dispatch its next kickoff until this returns.
            // Settlement has closed runtime admission. Clear any accepted but
            // unconsumed native input left by cancellation before a successor.
            // An uncertain cleanup fences the kickoff rather than risking replay.
            if let Err(error) = self.checked("clear_queue", json!({})) {
                self.shared.turn_images.lock().unwrap().clear();
                return Err(Error::unknown(format!(
                    "Pi native input cleanup was not confirmed: {error}"
                )));
            }
        }
        let interrupted_here = self.shared.interrupt_at.lock().unwrap().take().is_some();
        let master = self.shared.master.load(Ordering::SeqCst);
        let settled = settle_pi_turn(&mut acc, master, interrupted_here);
        if settled.limited {
            // Durable failure record. Tool bodies stay on the thread
            // entries already recorded; this event only classifies the
            // refusal and states that the turn was not replayed.
            self.shared.emit(
                "cadence/serialized_body_limited",
                &json!({"turn": turn_id, "replayed": false}),
            );
        }
        *self.shared.rejected_turn.lock().unwrap() = settled.reset_session.then(|| turn_id.clone());
        if !settled.reset_session {
            self.shared.turn_images.lock().unwrap().clear();
        }
        Ok(TurnResult {
            turn_id,
            status: settled.status.to_string(),
            text: acc.text.unwrap_or_default(),
            stop_reason: acc.stop,
            error: settled.error,
        })
    }

    /// Slice 1 brokers no provider-initiated requests — extension UI
    /// dialogs are auto-cancelled and recorded.
    fn respond(&self, _request_id: &Value, _result: Value) -> Result<()> {
        Err(Error::rejected(
            "managed pi endpoints broker no requests in slice 1 — extension UI \
             dialogs are auto-cancelled and recorded as pi_ui_request events",
        ))
    }

    /// Pi's own `abort` — the turn ends with `stopReason:"aborted"`
    /// then `agent_settled`; the waiter bounds the hang to
    /// `INTERRUPT_GRACE`. The write never blocks; only when stdin is
    /// gone, full or busy does it fall back to SIGINT on the provider's
    /// own process group.
    fn interrupt(&self) {
        *self.shared.interrupt_at.lock().unwrap() = Some(Instant::now());
        let transport = self.transport.read().unwrap().clone();
        if transport.try_send(json!({"type": "abort"})).is_err() {
            transport.interrupt();
        }
    }

    /// Interrupt `turn_id` only while it is the turn in flight.
    fn interrupt_turn(
        &self,
        turn_id: &str,
        _settle: &dyn Fn() -> Result<bool>,
    ) -> Result<super::InterruptOutcome> {
        // Held across the send: `run_turn` clears the active turn under
        // this lock, so an interrupt can never land on the next turn.
        let active = self.shared.active_turn.lock().unwrap();
        if active.as_deref() != Some(turn_id) {
            return Ok(super::InterruptOutcome::NotRunning);
        }
        self.interrupt();
        Ok(super::InterruptOutcome::Delivered)
    }

    fn native_turn_steering(&self) -> bool {
        self.shared.guard_ready.load(Ordering::SeqCst) && !self.shared.dead.load(Ordering::SeqCst)
    }

    fn steer_turn(
        &self,
        turn_id: &str,
        text: &str,
        client_message_id: &str,
    ) -> Result<super::SteerOutcome> {
        if !self.native_turn_steering() {
            return Err(Error::rejected(
                "Pi runtime has no verified turn-input guard",
            ));
        }
        let active = self.shared.active_turn.lock().unwrap();
        if active.as_deref() != Some(turn_id) {
            return Ok(super::SteerOutcome::NotRunning);
        }
        let proof = self.guard_command("steer", turn_id, client_message_id, Some(text))?;
        match proof.get("outcome").and_then(Value::as_str) {
            Some("queued") => Ok(super::SteerOutcome::Queued),
            Some("skipped_inactive") => Ok(super::SteerOutcome::NotRunning),
            Some("rejected") => Err(Error::rejected("Pi runtime refused turn-bound guidance")),
            _ => Err(Error::unknown(
                "Unrecognized Pi runtime turn-input disposition",
            )),
        }
    }

    /// CAD-551: the operator's provider-session verbs. `stop` rides the
    /// daemon's `interrupt` path rather than this one.
    fn session_commands(&self) -> &'static [&'static str] {
        &[
            "state", "stats", "models", "model", "levels", "effort", "compact", "new", "stop",
        ]
    }

    fn session_command(&self, command: &str, arg: Option<&str>) -> Result<Value> {
        match command {
            "state" => self.checked("get_state", json!({})),
            "stats" => self.checked("get_session_stats", json!({})),
            "models" => self.checked("get_available_models", json!({})),
            "levels" => self.checked("get_available_thinking_levels", json!({})),
            // Bare `/model` lists — the same answer `models` gives.
            "model" => match arg {
                Some(a) => self.session_set_model(a),
                None => self.checked("get_available_models", json!({})),
            },
            // Bare `/effort`: the levels plus the live one.
            "effort" => match arg {
                Some(a) => self.session_set_effort(a),
                None => {
                    let levels = self.checked("get_available_thinking_levels", json!({}))?;
                    let state = self.checked("get_state", json!({}))?;
                    Ok(json!({
                        "levels": levels.get("levels").cloned().unwrap_or(Value::Null),
                        "current": state.get("thinkingLevel").cloned().unwrap_or(Value::Null),
                    }))
                }
            },
            "compact" => self.checked("compact", json!({})),
            "new" => {
                let out = self.checked("new_session", json!({}))?;
                // The session id changed — answer the fresh state so the
                // caller's header lands on it.
                let state = self.checked("get_state", json!({}))?;
                Ok(json!({"new_session": out, "state": state}))
            }
            other => Err(Error::rejected(format!(
                "the '{other}' command is not supported by pi"
            ))),
        }
    }

    /// CAD-1076: a master session whose history the provider refuses
    /// stays refused on every later call, so the turn it refused before
    /// any work is answered with a fresh provider session (once).
    fn reset_rejected_session(&self, result: &TurnResult) -> Result<bool> {
        let mut rejected = self.shared.rejected_turn.lock().unwrap();
        if rejected.as_deref() != Some(result.turn_id.as_str()) {
            return Ok(false);
        }
        *rejected = None;
        drop(rejected);
        // A reset that did not happen is no reset: the turn keeps the
        // provider's own error, and the daemon does not retry.
        if let Err(e) = self.checked("new_session", json!({})) {
            self.shared.turn_images.lock().unwrap().clear();
            self.shared.emit(
                "cadence/pi_session_reset_failed",
                &json!({"turn": result.turn_id, "error": e.to_string()}),
            );
            return Ok(false);
        }
        Ok(true)
    }

    fn disconnected(&self) -> bool {
        self.transport.read().unwrap().disconnected()
    }

    /// EOF on stdin is a clean shutdown to `pi --mode rpc`; the
    /// transport's TERM→KILL sequence covers a stubborn child. The
    /// owner-initiated teardown marks the endpoint dead itself — the
    /// reader's trailing EOF is then a tagged stale disconnect that can
    /// never kill a later `open` (I4).
    fn close(&self) {
        let transport = self.transport.read().unwrap().clone();
        transport.close_stdin();
        // A console attachment has no local child; its reader thread observes
        // EOF once the owner closes the console.
        transport.wait_exit(Duration::from_secs(3));
        transport.close();
        self.shared.on_disconnect();
        // CAD-1085: detach first, then stop exactly this agent's owned runtime.
        let attached = self.protected_caller.lock().unwrap().take();
        let uid = self.protected_agent_uid.lock().unwrap().take();
        if attached.is_some() {
            if let (Some(uid), Ok(provider)) =
                (uid, crate::daemon::protected_runtime::console_provider())
            {
                provider.stop_agent(&uid, "agent closed");
            }
        }
    }
}

impl PiAdapter {
    /// Bind the durable model task before a protected turn's prompt.
    fn bind_native_task(
        &self,
        client_message_id: &str,
        turn_token: &str,
    ) -> Result<Option<Box<dyn crate::daemon::protected_runtime::NativeTaskGuard>>> {
        let Some(caller) = self.protected_caller.lock().unwrap().clone() else {
            return Ok(None);
        };
        let provider = crate::daemon::protected_runtime::console_provider()?;
        Ok(Some(provider.bind_native_task(
            &caller,
            client_message_id,
            client_message_id,
            turn_token,
        )?))
    }
}

#[cfg(test)]
#[path = "pi/cad1195_acceptance.rs"]
mod cad1195_acceptance;
#[cfg(all(test, debug_assertions, unix))]
mod cad1553_company_authority_acceptance {
    use super::*;
    use crate::platform::deployments::DeploymentMetadata;
    use serde_json::json;

    const EXACT: &str = r#"{"schema":1,"master_trust_boundary":"company-container@1","providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;
    const ORDINARY: &str = r#"{"schema":1,"providers":[{"provider":"agenticos","origin":"http://api.internal","manifest_pin":"agenticos-manifest@1/publish_post@2"}]}"#;

    fn master(params: Value, state: &Path) -> Agent {
        Agent {
            alias: "master".into(),
            uid: "agt-cad1553".into(),
            provider: "pi".into(),
            endpoint_kind: "managed".into(),
            role: "worker".into(),
            team_role: None,
            cwd: state.to_string_lossy().into_owned(),
            sandbox: "read-only".into(),
            instructions: None,
            thread_id: None,
            session_id: None,
            model: None,
            effort: None,
            pid: None,
            pid_start: None,
            endpoint: None,
            params: Some(params),
            model_selection: None,
            quota: None,
            generation: None,
            state: "starting".into(),
            enabled: true,
            gc_keep: false,
            archived_at: None,
            archived_by: None,
            archive_reason: None,
            lifecycle_rev: 0,
            error: None,
            created: 0.0,
            updated: 0.0,
        }
    }

    fn plan(
        env: &ProviderEnv,
        state: &Path,
        agent: &Agent,
    ) -> Result<(Vec<String>, Option<crate::confine::Policy>)> {
        launch_command(env, state, agent)
    }

    fn inner(env: &ProviderEnv, state: &Path, agent: &Agent) -> Vec<String> {
        build_command(env, agent, state).unwrap()
    }

    fn refused(env: &ProviderEnv, state: &Path, agent: &Agent, case: &str) {
        let error = plan(env, state, agent)
            .expect_err(&format!("{case} manufactured company file authority"));
        assert!(
            error.to_string().contains(
                "cannot isolate operational upload workspace without effective Master confinement"
            ),
            "{case} refused for an unrelated reason: {error}"
        );
    }

    #[test]
    fn mutable_values_cannot_grant_company_authority_but_exact_image_proof_can() {
        let root = tempfile::Builder::new()
            .prefix("c1553-pi-")
            .tempdir()
            .unwrap();
        let state = root.path().join("s");
        let tracker = root.path().join("t");
        let home = root.path().join("h");
        let private = root.path().join("p");
        let workspace = private.join("workspace");
        for path in [&state, &home, &workspace] {
            std::fs::create_dir_all(path).unwrap();
        }
        crate::issue::Pm::init(&tracker).unwrap();
        let store = Arc::new(crate::store::Store::open(&state.join("cadence.sqlite3")).unwrap());
        let mut settings = store.import_legacy_settings(&tracker).unwrap();
        settings.doc.pi = Some(
            serde_json::from_value(json!({
                "models": { "allow": ["fixture/model"] }
            }))
            .unwrap(),
        );
        store
            .save_settings(settings.revision, settings.doc, "cad1553-fixture")
            .unwrap();
        let mutable = ProviderEnv::refusing_providers().with_settings_store(Arc::clone(&store));
        mutable.set("HOME", home.to_str().unwrap());
        mutable.set("CADENCE_HOME", private.to_str().unwrap());
        mutable.set("CADENCE_PM_DIR", tracker.to_str().unwrap());
        mutable.set("CADENCE_CONFINE_COMMAND", "/bin/false");
        mutable.set("CADENCE_PI_COMMAND", "/bin/false");
        mutable.set("CADENCE_CLAUDE_COMMAND", "/bin/false");
        mutable.set(crate::master::CONFINE_EXTRA_READ_ENV, "");
        mutable.set(crate::master::CONFINE_EXTRA_WRITE_ENV, "");
        // A real, fixture-owned absolute extension path avoids relying on the
        // test runner having no installed image extension in its ambient env.
        let extension = root.path().join("image-read-extension.js");
        std::fs::write(&extension, "// command-planning fixture only\n").unwrap();
        mutable.set(
            "CADENCE_PI_AGENTICOS_READ_EXTENSION",
            extension.to_str().unwrap(),
        );
        mutable.set(crate::master::TEST_NO_LANDLOCK, "1");
        assert!(
            crate::master::confinement_available(&mutable).is_err(),
            "the real per-daemon no-Landlock seam must be effective"
        );
        let bound = mutable.with_workspace(workspace.clone());
        let loose = master(json!({"unconfined":true,"model":"fixture/model"}), &state);
        refused(&bound, &state, &loose, "absent authority");
        let ordinary = DeploymentMetadata::parse(ORDINARY.as_bytes()).unwrap();
        refused(
            &bound
                .clone()
                .with_company_master(ordinary.hosted_company_master()),
            &state,
            &loose,
            "ordinary provider metadata",
        );

        // These are hostile values, deliberately supplied below the registry
        // validator: even an in-memory Agent with forged fields cannot grant
        // the adapter authority. This is not a test-only granting boolean.
        let forged = master(
            json!({
                "unconfined":true,"model":"fixture/model",
                "master_trust_boundary":"company-container@1",
                "company_master":true,"hosted_company_master":true,
                "provider_deployments":serde_json::from_str::<Value>(EXACT).unwrap()
            }),
            &state,
        );
        refused(&bound, &state, &forged, "forged agent params alone");
        let claims = [
            ("master_trust_boundary", "company-container@1"),
            ("CADENCE_MASTER_TRUST_BOUNDARY", "company-container@1"),
            ("CADENCE_COMPANY_MASTER", "true"),
            ("CADENCE_HOSTED_COMPANY_MASTER", "true"),
            ("CADENCE_PROVIDER_DEPLOYMENTS", EXACT),
        ];
        for (key, value) in claims {
            mutable.set(key, value);
        }
        refused(&bound, &state, &loose, "forged mutable env alone");
        refused(
            &bound.clone(),
            &state,
            &forged,
            "forged mutable env plus params",
        );

        let image = DeploymentMetadata::parse(EXACT.as_bytes()).unwrap();
        let trusted = bound
            .clone()
            .with_company_master(image.hosted_company_master());
        for (case, env) in [
            ("exact image", trusted.clone()),
            (
                "proof survives clone and checked workspace binding",
                trusted.clone().with_workspace(workspace.clone()),
            ),
        ] {
            let (argv, policy) = plan(&env, &state, &loose).unwrap_or_else(|error| {
                panic!("{case} refused existing unconfined Master: {error}")
            });
            assert!(
                policy.is_none(),
                "{case} falsely emitted a confinement policy"
            );
            assert!(
                !master_confined(&env, &loose),
                "{case} falsely labeled an unconfined Master confined"
            );
            assert_eq!(
                argv,
                inner(&env, &state, &loose),
                "{case} changed provider command instead of lifting only workspace refusal"
            );
        }

        // vars() is the actual environment handoff. Re-importing every value
        // must lose the proof rather than serializing company authority.
        let replayed = ProviderEnv::default()
            .with_workspace(workspace.clone())
            .with_settings_store(Arc::clone(&store));
        for (key, value) in trusted.vars() {
            replayed.set(&key, value);
        }
        refused(&replayed, &state, &loose, "serialized env replay");
        for (key, _) in claims {
            mutable.remove(key);
        }
        assert!(
            !bound.company_master(),
            "mutable-map clone acquired the trusted sibling proof"
        );
        assert!(
            trusted.company_master(),
            "mutable-map changes erased image-owned proof"
        );
        let (argv, policy) = plan(&trusted, &state, &loose).unwrap();
        assert!(policy.is_none());
        assert_eq!(argv, inner(&trusted, &state, &loose));
        refused(&bound, &state, &loose, "env claims removed again");

        // Image authority is not an implicit unconfined opt-in. With the
        // existing opt-in absent, the launch still uses cadence confine;
        // on this simulated host that wrapper will fail closed at execution.
        let not_opted_in = master(json!({"model":"fixture/model"}), &state);
        let (argv, policy) = plan(&trusted, &state, &not_opted_in).unwrap();
        assert!(
            policy.is_some(),
            "image assertion silently opted Master out of confinement"
        );
        assert_eq!(
            &argv[..2],
            &["/bin/false".to_string(), "confine".to_string()]
        );

        // Removing only the unavailable-host seam restores actual host
        // availability. Where effective confinement exists it must win even
        // with both valid image proof and the saved unconfined opt-in.
        mutable.remove(crate::master::TEST_NO_LANDLOCK);
        let available = crate::master::confinement_available(&trusted).is_ok();
        let (argv, policy) = plan(&trusted, &state, &loose).unwrap();
        assert_eq!(
            policy.is_some(),
            available,
            "company trust overrode effective confinement or falsely claimed it"
        );
        assert_eq!(master_confined(&trusted, &loose), available);
        if let Some(policy) = policy {
            let expected = crate::master::confine_argv(
                "/bin/false",
                &policy,
                &inner(&trusted, &state, &loose),
            );
            assert_eq!(argv, expected);
        } else {
            assert_eq!(argv, inner(&trusted, &state, &loose));
        }
    }
}

#[cfg(test)]
mod cad1247_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// CAD-1247: the resolver the daemon-start warning and
    /// `doctor --host` share — bare `pi` resolves on PATH, names the
    /// PATH when absent; `CADENCE_PI_COMMAND` names its own program.
    #[test]
    fn pi_launch_command_resolution() {
        let env = ProviderEnv::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap().to_string();
        assert!(resolve_pi_launch_command(&env, Some(&path)).is_err());
        let pi = dir.path().join("pi");
        std::fs::write(&pi, "#!/bin/sh\n").unwrap();
        let mut mode = pi.metadata().unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(&pi, mode).unwrap();
        assert_eq!(
            resolve_pi_launch_command(&env, Some(&path)).as_deref().ok(),
            Some(pi.as_path())
        );
        let relative_dir = tempfile::Builder::new()
            .prefix(".cad1247-resolution-")
            .tempdir_in(".")
            .unwrap();
        let relative_root = Path::new(".").join(relative_dir.path().file_name().unwrap());
        let relative_pi = relative_root.join("pi");
        std::fs::copy(&pi, &relative_pi).unwrap();
        let relative_path = relative_root.to_str().unwrap();
        assert!(!relative_pi.is_absolute());
        assert!(resolve_pi_launch_command(&env, Some(relative_path))
            .unwrap_err()
            .contains("indeterminate"));
        assert_eq!(
            resolve_pi_launch_command(&env, Some(&format!("{path}:{relative_path}"))).unwrap(),
            pi
        );
        assert!(
            resolve_pi_launch_command(&env, Some(&format!("{relative_path}:{path}")))
                .unwrap_err()
                .contains("indeterminate")
        );
        env.set("CADENCE_PI_COMMAND", relative_pi.to_str().unwrap());
        assert!(resolve_pi_launch_command(&env, Some(&path))
            .unwrap_err()
            .contains("indeterminate"));
        env.set("CADENCE_PI_COMMAND", "/missing/pi --flag");
        assert!(resolve_pi_launch_command(&env, Some(&path)).is_err());
        env.set("CADENCE_PI_COMMAND", "/bin/true --flag");
        assert_eq!(
            resolve_pi_launch_command(&env, Some(&path)).as_deref().ok(),
            Some(Path::new("/bin/true"))
        );
    }
}

#[cfg(test)]
mod cad1098_tests {
    use super::*;
    use crate::master::Profile::{App, Home};

    fn heads(rules: &[Value]) -> Vec<String> {
        rules
            .iter()
            .map(|r| r["argv"][0].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// CAD-1098 Gate 1 (acceptance): the Pi guard's rules are chosen by
    /// the session profile, and the file `open` writes for each session
    /// carries ONLY that profile's rules — an app session's guard, then a
    /// Home one over it, then an app one again: nothing carries over.
    ///
    /// Guard: `master::allowed_tools(profile)` in `pi_guard_rules`, and the
    /// `profile` threaded through `write_pi_guard`.
    #[test]
    fn cad1098_pi_guard_rules_follow_the_session_profile() {
        let (home, app) = (pi_guard_rules(Home), pi_guard_rules(App));
        assert!(heads(&home).iter().any(|h| h == "issue"));
        assert!(!app.is_empty());
        assert!(
            // CAD-1168: `attachment read` rides both profiles.
            heads(&app)
                .iter()
                .all(|h| h == "app" || h == "attachment" || h == "artifact"),
            "{app:?}"
        );
        assert!(app.iter().all(|r| home.contains(r)));
        let dir = tempfile::Builder::new().prefix("c98p").tempdir().unwrap();
        let rules_in = |path: &Path| -> String {
            let text = std::fs::read_to_string(path).unwrap();
            text.lines()
                .find(|l| l.contains("\"argv\""))
                .unwrap_or_default()
                .to_string()
        };
        let path = write_pi_guard(dir.path(), false, App).unwrap();
        let first = rules_in(&path);
        assert!(first.contains("\"argv\":[\"app\"") && !first.contains("\"argv\":[\"issue\""));
        write_pi_guard(dir.path(), false, Home).unwrap();
        assert!(rules_in(&path).contains("\"argv\":[\"issue\""));
        write_pi_guard(dir.path(), false, App).unwrap();
        assert_eq!(
            rules_in(&path),
            first,
            "the Home rules carried into the app guard"
        );
    }
}

#[cfg(test)]
mod cad1457_tests {
    use super::*;
    use super::{AdapterHooks, ProviderAdapter};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, Mutex};

    const USER: &str = "keep this user input";
    const GENERIC_BODY: &str =
        "400: {\"message\":\"Request body exceeds 100 bytes.\",\"code\":\"invalid_request\"}";
    const TOKEN: &str = "400: {\"message\":\"maximum context length is 262144 tokens\",\"code\":\"invalid_request\"}";

    struct Stop {
        adapter: Option<PiAdapter>,
        dir: Option<tempfile::TempDir>,
    }

    type ObservedEvents = Arc<Mutex<Vec<(String, Value)>>>;
    type Launch = (Stop, std::path::PathBuf, ObservedEvents);

    impl Drop for Stop {
        fn drop(&mut self) {
            if let Some(adapter) = self.adapter.take() {
                adapter.close();
            }
            self.dir.take();
        }
    }

    fn hooks(events: Arc<Mutex<Vec<(String, Value)>>>) -> AdapterHooks {
        AdapterHooks {
            on_event: Box::new(move |method, params| {
                events
                    .lock()
                    .unwrap()
                    .push((method.to_string(), params.clone()));
            }),
            on_request: Box::new(|_| {}),
        }
    }

    fn script(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("pi-rpc");
        let mut file = std::fs::File::create(&path).unwrap();
        write!(
            file,
            r#"#!/usr/bin/python3
import json, os, sys
mode = os.environ.get("CAD1457_MODE", "after")
error = os.environ.get("CAD1457_ERROR", "")
log_path = os.environ["CAD1457_LOG"]

def record(req):
    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(req, separators=(",", ":")) + "\n")

def emit(obj):
    sys.stdout.write(json.dumps(obj, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def respond(req):
    emit({{"id": req.get("id"), "type": "response", "command": req.get("type"), "success": True}})

def fail_turn(stop):
    emit({{
        "type": "message_end",
        "message": {{
            "role": "assistant",
            "content": [{{"type": "text", "text": "partial answer"}}],
            "stopReason": stop,
            "errorMessage": error,
        }},
    }})
    emit({{"type": "agent_settled"}})

for raw in sys.stdin:
    raw = raw.strip()
    if not raw:
        continue
    try:
        req = json.loads(raw)
    except json.JSONDecodeError:
        continue
    record(req)
    kind = req.get("type")
    if kind == "prompt":
        respond(req)
        if mode == "cancel":
            continue
        if mode == "after":
            emit({{
                "type": "tool_execution_start",
                "toolName": "bash",
                "toolCallId": "tool-1",
                "args": {{"command": "cadence wiki put agents/master/knowledge/note.md --file /tmp/note"}},
            }})
            emit({{
                "type": "tool_execution_end",
                "toolCallId": "tool-1",
                "isError": False,
                "result": {{"content": [{{"type": "text", "text": "wrote"}}]}},
            }})
        fail_turn("error")
    elif kind == "abort":
        fail_turn("aborted")
    else:
        respond(req)
"#
        )
        .unwrap();
        let mut mode = file.metadata().unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(&path, mode).unwrap();
        path
    }

    fn launch(mode: &str, error: &str, master: bool) -> Launch {
        let dir = tempfile::tempdir().unwrap();
        let script = script(dir.path());
        let log = dir.path().join("stderr.log");
        let commands = dir.path().join("commands.jsonl");
        let events = Arc::new(Mutex::new(Vec::new()));
        let env = ProviderEnv::default();
        let adapter = PiAdapter::new(hooks(Arc::clone(&events)), &log, &env, None);
        let generation = "cad1457".to_string();
        *adapter.shared.generation.lock().unwrap() = generation.clone();
        adapter.shared.master.store(master, Ordering::SeqCst);
        adapter.shared.dead.store(false, Ordering::SeqCst);
        adapter.shared.guard_ready.store(false, Ordering::SeqCst);
        *adapter.shared.idle_window.lock().unwrap() = Duration::from_secs(5);
        let transport =
            adapter.transport_for(&[script.to_string_lossy().to_string()], master, &generation);
        transport
            .launch(
                dir.path().to_str().unwrap(),
                &log,
                &[
                    ("CAD1457_MODE".to_string(), mode.to_string()),
                    ("CAD1457_ERROR".to_string(), error.to_string()),
                    (
                        "CAD1457_LOG".to_string(),
                        commands.to_string_lossy().to_string(),
                    ),
                ],
            )
            .unwrap();
        *adapter.transport.write().unwrap() = transport;
        (
            Stop {
                adapter: Some(adapter),
                dir: Some(dir),
            },
            commands,
            events,
        )
    }

    fn commands(path: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn count(frames: &[Value], kind: &str) -> usize {
        frames
            .iter()
            .filter(|frame| frame.get("type").and_then(Value::as_str) == Some(kind))
            .count()
    }

    #[test]
    fn after_tool_body_rejection_does_not_submit_the_tool_again() {
        let (stop, log, events) = launch("after", GENERIC_BODY, true);
        let adapter = stop.adapter.as_ref().unwrap();
        let result = adapter.run_turn(USER, "m1", &|_| {}).unwrap();
        assert_eq!(result.status, "failed");
        assert_eq!(result.text, "partial answer");
        let error = result.error.as_deref().unwrap();
        assert!(error.contains(GENERIC_BODY));
        assert!(!error.contains("256"));
        assert!(!error.contains("KiB"));
        assert!(!adapter.reset_rejected_session(&result).unwrap());
        let frames = commands(&log);
        assert_eq!(count(&frames, "prompt"), 1);
        assert_eq!(count(&frames, "new_session"), 0);
        assert!(frames.iter().any(|frame| {
            frame.get("type").and_then(Value::as_str) == Some("prompt")
                && frame
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(USER))
        }));
        let seen = events.lock().unwrap();
        assert!(seen.iter().any(|(method, _)| method == "cadence/tool_use"));
        assert!(seen
            .iter()
            .any(|(method, _)| method == "cadence/tool_result"));
        assert!(seen.iter().any(|(method, params)| {
            method == "cadence/serialized_body_limited"
                && params.get("replayed").and_then(Value::as_bool) == Some(false)
        }));
        assert!(adapter.shared.master.load(Ordering::SeqCst));
    }

    #[test]
    fn before_tool_body_rejection_still_resets_once() {
        let (stop, log, events) = launch("before", GENERIC_BODY, true);
        let adapter = stop.adapter.as_ref().unwrap();
        let result = adapter.run_turn(USER, "m1", &|_| {}).unwrap();
        assert_eq!(result.status, "failed");
        assert_eq!(result.error.as_deref(), Some(GENERIC_BODY));
        assert!(adapter.reset_rejected_session(&result).unwrap());
        let frames = commands(&log);
        assert_eq!(count(&frames, "prompt"), 1);
        assert_eq!(count(&frames, "new_session"), 1);
        assert!(!events
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| { method == "cadence/serialized_body_limited" }));
        let (worker, worker_log, _) = launch("before", GENERIC_BODY, false);
        let worker_adapter = worker.adapter.as_ref().unwrap();
        let worker_result = worker_adapter.run_turn(USER, "m2", &|_| {}).unwrap();
        assert_eq!(worker_result.error.as_deref(), Some(GENERIC_BODY));
        assert!(!worker_adapter
            .reset_rejected_session(&worker_result)
            .unwrap());
        assert!(!worker_adapter.shared.master.load(Ordering::SeqCst));
        assert_eq!(count(&commands(&worker_log), "new_session"), 0);
    }

    #[test]
    fn token_refusal_is_not_treated_as_a_body_limitation() {
        let (stop, _, _) = launch("before", TOKEN, true);
        let adapter = stop.adapter.as_ref().unwrap();
        let before = adapter.run_turn(USER, "m1", &|_| {}).unwrap();
        assert_eq!(before.status, "failed");
        assert_eq!(before.error.as_deref(), Some(TOKEN));
        assert!(adapter.reset_rejected_session(&before).unwrap());

        let (stop, log, events) = launch("after", TOKEN, true);
        let adapter = stop.adapter.as_ref().unwrap();
        let after = adapter.run_turn(USER, "m2", &|_| {}).unwrap();
        assert_eq!(after.status, "failed");
        assert_eq!(after.error.as_deref(), Some(TOKEN));
        assert!(!adapter.reset_rejected_session(&after).unwrap());
        assert_eq!(count(&commands(&log), "prompt"), 1);
        assert_eq!(count(&commands(&log), "new_session"), 0);
        assert!(!events
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| { method == "cadence/serialized_body_limited" }));
    }

    #[test]
    fn cancellation_does_not_reset_or_continue() {
        let (stop, log, events) = launch("cancel", GENERIC_BODY, true);
        let adapter = stop.adapter.as_ref().unwrap();
        let result = adapter
            .run_turn(USER, "m1", &|_| adapter.interrupt())
            .unwrap();
        assert_eq!(result.status, "interrupted");
        assert!(result.error.is_none());
        assert!(!adapter.reset_rejected_session(&result).unwrap());
        let frames = commands(&log);
        assert_eq!(count(&frames, "prompt"), 1);
        assert_eq!(count(&frames, "abort"), 1);
        assert_eq!(count(&frames, "new_session"), 0);
        assert!(!events
            .lock()
            .unwrap()
            .iter()
            .any(|(method, _)| { method == "cadence/serialized_body_limited" }));
    }
}
