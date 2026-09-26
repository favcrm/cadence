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
//! - Sessions are disposable by design (the brief): the process is
//!   launched `--no-session`, so every `open` mints a fresh session id
//!   and the daemon rebuilds context from the continuity pack (CAD-324)
//!   instead of resuming provider state.
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

/// The only inherited variables a Pi MASTER keeps (CAD-322 round 2,
/// I3): its environment is cleared wholesale and only these names are
/// re-read — connectivity and locale, never credentials. Every provider
/// key (AWS_*, Moonshot/Kimi, Together, Fireworks, Baseten, Llama,
/// NVIDIA, OpenCode, Minimax, Qwen, Xiaomi, ZAI_CN, Ant Ling,
/// ANTHROPIC_AUTH_TOKEN, COPILOT_GITHUB_TOKEN, …), every `PI_*`,
/// `CLAUDE_*`, `CADENCE_*`, `GIT_*`, `XDG_*`, `LD_*`, `NODE_OPTIONS`
/// and `SSH_AUTH_SOCK` is gone however it is spelled — a denylist could
/// never enumerate the next provider's variable. The daemon context and
/// `PI_CODING_AGENT_DIR` arrive as explicit env pairs afterwards.
const PI_MASTER_ENV_KEEP: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TERM",
    "COLORTERM",
    "SHELL",
    "TZ",
    // Connectivity config, not secrets: corporate CA roots and proxies.
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

/// The master's environment posture: cleared, then the allowlist.
/// `DENIED_ENV`'s forge tokens are re-listed only as defense in depth —
/// they are not in the keep set anyway.
pub(crate) fn pi_master_env_scrub() -> EnvScrub {
    EnvScrub::cleared_except(PI_MASTER_ENV_KEEP).and_names(crate::master::DENIED_ENV)
}

/// Worker posture (non-master aliases): the old prefix rule still
/// applies — every inherited `PI_*`, `CADENCE_*`, `CLAUDE_*` and
/// `CODEX_*` is removed (a `PI_*` or `CLAUDE_*` leak could silently
/// retarget the child's config or identity); the real pair
/// (`CADENCE_ALIAS`, `CADENCE_STATE_DIR`) and the daemon context are
/// re-injected per agent. Provider API keys and cloud-secret names are
/// always dropped.
pub(crate) fn pi_env_scrub() -> EnvScrub {
    EnvScrub::prefixes(&["PI_", "CADENCE_", "CLAUDE_", "CLAUDECODE", "CODEX_"], &[])
        .and_names(PI_KEY_ENV)
        .and_names(super::CLOUD_SECRET_ENV)
}

/// Provider API-key variables a Pi WORKER must not inherit from the
/// daemon's environment — the master's child env is allowlisted
/// wholesale ([`pi_master_env_scrub`]) instead. Scrubbed by name: Pi
/// reads a fixed set, none of them `*_API_KEY`-suffixed generically.
const PI_KEY_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_OAUTH_TOKEN",
    "OPENAI_API_KEY",
    "OPENAI_API_KEY_OAUTH",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "XAI_API_KEY",
    "OPENROUTER_API_KEY",
    "CEREBRAS_API_KEY",
    "DEEPSEEK_API_KEY",
    "ZAI_API_KEY",
    "AI_GATEWAY_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AZURE_OPENAI_API_KEY",
    // Round-2 review set: vendor keys Pi reads that the original list
    // missed (the master no longer depends on this list being complete).
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "MOONSHOT_API_KEY",
    "KIMI_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "BASETEN_API_KEY",
    "LLAMA_API_KEY",
    "NVIDIA_API_KEY",
    "OPENCODE_API_KEY",
    "MINIMAX_API_KEY",
    "QWEN_API_KEY",
    "DASHSCOPE_API_KEY",
    "XIAOMI_API_KEY",
    "ZAI_CN_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "COPILOT_GITHUB_TOKEN",
];

/// Provider binary; `CADENCE_PI_COMMAND` overrides it (test/mock) —
/// the mock sees the same argv shape as the real CLI.
fn pi_command(env: &ProviderEnv) -> Vec<String> {
    if let Some(cmd) = env.var("CADENCE_PI_COMMAND") {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if !parts.is_empty() {
            return parts;
        }
    }
    vec!["pi".to_string()]
}

/// The master's own Pi config dir — its `PI_CODING_AGENT_DIR`: auth
/// and extension state live here, never in the operator's `~/.pi`,
/// mirroring [`crate::master::claude_config_dir`].
pub fn pi_config_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("pi")
}

/// The generated guard extension path (`<state>/master/pi-guard.js`).
/// Plain `.js` — Pi's loader (jiti) accepts it, and Node can evaluate
/// the grammar section verbatim in tests. It must live OUTSIDE
/// `master/pi`: that dir is writable by the confined master, and a
/// writable guard could rewrite itself.
fn pi_guard_path(state_dir: &Path) -> PathBuf {
    state_dir.join("master").join("pi-guard.js")
}

/// `pi --mode rpc --no-session …`: direct argv, no shell. The master
/// additionally gets `--no-extensions -e <guard>` and `--tools bash` —
/// its whole toolset is the allowlisted `cadence` commands, exactly the
/// CAD-339 posture the Claude master gets from `--tools`/`dontAsk`.
/// `--offline`/`--no-*` keep startup deterministic: no update checks,
/// no skills/prompt-templates/themes/context-file discovery.
fn build_command(env: &ProviderEnv, agent: &Agent, state_dir: &Path) -> Vec<String> {
    let params = agent.params.clone().unwrap_or(Value::Null);
    let mut cmd = pi_command(env);
    for flag in ["--mode", "rpc", "--no-session", "--offline", "--no-themes"] {
        cmd.push(flag.to_string());
    }
    for flag in ["--no-skills", "--no-prompt-templates", "--no-context-files"] {
        cmd.push(flag.to_string());
    }
    // `agent.model` is the provider's report, never a launch param —
    // only the configured param is replayed (same rule as Claude).
    if let Some(model) = params.get("model").and_then(Value::as_str) {
        cmd.extend(["--model".to_string(), model.to_string()]);
    }
    if crate::master::is_master(&agent.alias) {
        cmd.extend([
            "--no-extensions".to_string(),
            "--extension".to_string(),
            pi_guard_path(state_dir).to_string_lossy().to_string(),
            "--tools".to_string(),
            // bash (guard-checked `cadence` verbs) plus `read` — the
            // guard confines reads to `master/tmp`, where Pi spills long
            // bash output (CAD-552: re-read the spill, don't re-run).
            "bash,read".to_string(),
        ]);
    }
    cmd
}

/// The full launch argv: [`build_command`], and for the master that
/// line wrapped in `cadence confine` with its policy (CAD-439 — the
/// same Landlock boundary as the Claude master; nothing is widened
/// beyond Pi's own install tree and config dir).
fn launch_command(
    env: &ProviderEnv,
    state_dir: &Path,
    agent: &Agent,
) -> (Vec<String>, Option<crate::confine::Policy>) {
    let command = build_command(env, agent, state_dir);
    if !master_confined(env, agent) {
        return (command, None);
    }
    let (confine, policy) = pi_master_confinement(env, state_dir);
    let argv = crate::master::confine_argv(&confine, &policy, &command);
    (argv, Some(policy))
}

/// The master, confined wherever this host can confine it — the stored
/// `unconfined` param counts only where it cannot (same rule as Claude).
fn master_confined(env: &ProviderEnv, agent: &Agent) -> bool {
    crate::master::is_master(&agent.alias)
        && crate::master::is_confined(
            agent.params.as_ref(),
            crate::master::confinement_available(env).is_ok(),
        )
}

/// The confining binary and policy for a Pi master (`cadence master
/// confinement` prints the Claude one; Pi's differs only in the
/// programs and provider dirs it names).
pub fn pi_master_confinement(
    env: &ProviderEnv,
    state_dir: &Path,
) -> (String, crate::confine::Policy) {
    let command = pi_command(env);
    (
        confine_command(env),
        crate::master::confinement(&pi_confine_inputs(env, state_dir, &command)),
    )
}

fn confine_command(env: &ProviderEnv) -> String {
    #[cfg(debug_assertions)]
    let own = env
        .own("CADENCE_CONFINE_COMMAND")
        .filter(|c| !c.trim().is_empty());
    #[cfg(not(debug_assertions))]
    let own: Option<String> = {
        let _ = env;
        None
    };
    own.or_else(|| {
        std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    })
    .unwrap_or_else(|| "cadence".to_string())
}

/// The directory an npm-installed package lives in: `program`'s real
/// path's first ancestor holding a `package.json`. Pi is a node script
/// inside `…/node_modules/@earendil-works/pi-coding-agent`, so the bin
/// symlink's parent alone is not enough — the package dir is added to
/// the master's read set.
fn package_root(program: &Path) -> Option<PathBuf> {
    let real = program.canonicalize().ok()?;
    real.ancestors()
        .find(|dir| dir.join("package.json").is_file())
        .map(Path::to_path_buf)
}

/// What the Pi master's confinement is computed from — same shape as
/// Claude's inputs, plus Pi's package dir, the guard extension and its
/// private `PI_CODING_AGENT_DIR`.
fn pi_confine_inputs(
    env: &ProviderEnv,
    state_dir: &Path,
    command: &[String],
) -> crate::master::ConfineInputs {
    use crate::master::{split_paths, which, with_interpreter};
    let path = env.var("PATH");
    let mut programs = Vec::new();
    if let Some(p) = command.first().and_then(|c| which(c, path.as_deref())) {
        programs.extend(with_interpreter(&p, path.as_deref()));
        if let Some(root) = package_root(&p) {
            programs.push(root);
        }
    }
    if let Some(p) = which("cadence", path.as_deref()) {
        programs.push(p);
    }
    programs.push(PathBuf::from(confine_command(env)));
    let pm_dir = env
        .var("CADENCE_PM_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::issue::default_dir().ok());
    // The guard lives outside `master/pi` — a confined master must read
    // but never write it. Its own provider dir is `master/pi`: nothing
    // of `master/claude` (or its `.credentials.json`) is in the policy.
    let mut extra_read = split_paths(env.var(crate::master::CONFINE_EXTRA_READ_ENV));
    extra_read.push(pi_guard_path(state_dir));
    crate::master::ConfineInputs {
        state_dir: state_dir.to_path_buf(),
        home: env.var("HOME").filter(|h| !h.is_empty()).map(PathBuf::from),
        pm_dir,
        programs,
        provider_dir: pi_config_dir(state_dir),
        // Nothing of the operator's `$HOME` — Pi's npm package arrives
        // via `programs`; Claude's `~/.local/share/claude` is not ours.
        home_read: &[],
        extra_read,
        extra_write: split_paths(env.var(crate::master::CONFINE_EXTRA_WRITE_ENV)),
    }
}

/// The generated guard extension — the Pi analogue of
/// `CLAUDE_ALLOWED_TOOLS` + `--tools Bash` (CAD-322 round 2, C1). Pi's
/// bash tool takes a single command STRING that a shell parses, so a
/// string-prefix check is a bypass waiting to happen (`cadence status;
/// rm`, `cadence status | sh`, `FOO=x cadence …`). The check is a
/// grammar instead: every character must be in `[A-Za-z0-9 ._\/=:-]` —
/// `;`, `&`, `|`, `$`, backticks, `>`, `<`, `(`, `)`, quotes, newlines
/// and every control character are refused outright — then the command
/// tokenizes on whitespace, `argv[0]` must be `cadence` exactly (an
/// `A=b` env prefix or a `bash -c …` wrapper fails it), and `argv[1..]`
/// must match one allowlisted subcommand entry. The grammar section
/// between the markers is evaluated verbatim by `tests/pi_master.rs`
/// under Node — keep it dependency-free plain JavaScript.
const PI_GUARD_HEAD: &str = r#"// Generated by cadence (CAD-322) — do not edit.
// The master may run only the allowlisted `cadence` commands and read
// files under its own tmp dir; every other tool call and every other
// bash command is blocked, matching the Claude master's --tools Bash +
// Bash(cadence <verb>) posture.
// Plain JS: tests/pi_master.rs evaluates the grammar section verbatim.
export default function (pi) {
  // >>> cadence-guard-grammar >>>
  // A command is allowed iff it parses under a grammar, never a string
  // prefix: charset, tokenization, argv[0] === "cadence", then an
  // argv-prefix match on the allowlist. `args: true` entries are the
  // `Bash(cadence <verb…> *)` forms — they need at least one arg.
  const RULES = __RULES__;
  // The master's own tmp dir — Pi's bash spills long output here, so
  // the `read` tool is confined to it (CAD-552). Lexical normalization
  // suffices: the bash grammar runs only `cadence` verbs, so nothing
  // the master executes can plant a symlink inside MASTER_TMP.
  const MASTER_TMP = __MASTER_TMP__;
  // The refusal names the rule the command hit and the allowed forms —
  // the master reads it and self-corrects instead of probing (CAD-552).
  function piGuardRefusal(cmd) {
    if (typeof cmd !== "string" || cmd.length === 0 || cmd.length > 4096) {
      return "one `cadence …` command per call";
    }
    // Every metacharacter, quote, expansion, redirection, glob and
    // control character is refused by the charset — `;`, `&&`, `||`,
    // `|`, `$(`, backticks, `>`, `<`, `(`, `)`, `\n`, `"`, `'`, `\`.
    if (!/^[A-Za-z0-9 ._\/=:-]+$/.test(cmd)) {
      return "no pipes, chaining (`;`, `&&`, `||`), redirects, `$(…)`, backticks, quotes or escapes — run one plain `cadence …` and filter inside it (`--json`, `--status`, `--project`), never `head`/`grep`/`tail`/`jq`";
    }
    const argv = cmd.split(" ").filter((t) => t.length > 0);
    if (argv[0] !== "cadence") {
      return "every command starts `cadence` — no env prefixes (`FOO=bar cadence …`), wrappers (`bash -c …`) or other programs";
    }
    const rest = argv.slice(1);
    const allowed = RULES.some(
      (r) =>
        r.argv.every((tok, i) => rest[i] === tok) &&
        (r.args ? rest.length > r.argv.length : rest.length === r.argv.length),
    );
    if (allowed) {
      return null;
    }
    return (
      "not an allowlisted verb — allowed: " +
      RULES.map((r) => "cadence " + r.argv.join(" ") + (r.args ? " …" : "")).join("; ")
    );
  }
  function piGuardAllows(cmd) {
    return piGuardRefusal(cmd) === null;
  }
  // `read` is confined to MASTER_TMP: normalize `.`/`..` lexically,
  // resolve a relative path against cwd, then require the dir prefix —
  // `<tmp>-evil/x`, `<tmp>/../x` and `/etc/passwd` all refuse.
  function piReadPathAllowed(p) {
    if (typeof p !== "string" || p.length === 0) {
      return false;
    }
    const abs = p.startsWith("/") ? p : process.cwd() + "/" + p;
    const parts = [];
    for (const seg of abs.split("/")) {
      if (seg === "" || seg === ".") {
        continue;
      }
      if (seg === "..") {
        if (parts.length > 0) {
          parts.pop();
        }
        continue;
      }
      parts.push(seg);
    }
    const norm = "/" + parts.join("/");
    return norm === MASTER_TMP || norm.startsWith(MASTER_TMP + "/");
  }
  // <<< cadence-guard-grammar <<<
  pi.on("tool_call", async (event) => {
    if (event.toolName === "read") {
      const p = event.input && event.input.path;
      if (!piReadPathAllowed(p)) {
        return { block: true, reason: `cadence master: the read tool is confined to ${MASTER_TMP} (Pi's spilled bash output lives there) — got '${p}'` };
      }
      return;
    }
    if (event.toolName !== "bash") {
      return { block: true, reason: `cadence master: only allowlisted \`cadence\` bash commands and reads inside ${MASTER_TMP} are permitted (tool '${event.toolName}' is not enabled)` };
    }
    const cmd = String(event.input?.command ?? "");
    const refusal = piGuardRefusal(cmd);
    if (refusal !== null) {
      return { block: true, reason: `cadence master refused bash '${cmd}': ${refusal}` };
    }
  });
}
"#;

/// One allowlisted invocation in the generated guard: the argv after
/// `cadence`, and whether trailing arguments are allowed
/// (`Bash(cadence <verb…> *)` forms) or the match is exact.
fn pi_guard_rules() -> Vec<Value> {
    let mut rules = Vec::new();
    for tool in crate::master::CLAUDE_ALLOWED_TOOLS {
        let Some(inner) = tool.strip_prefix("Bash(").and_then(|t| t.strip_suffix(')')) else {
            continue;
        };
        let (stem, args) = match inner.strip_suffix(" *") {
            Some(stem) => (stem, true),
            None if inner.ends_with('*') => continue, // `foo*`-forms are never allowlisted
            None => (inner, false),
        };
        let argv: Vec<&str> = stem.split_whitespace().collect();
        if argv.first() != Some(&"cadence") || argv.len() < 2 {
            continue; // a rule that cannot start with `cadence` is dead weight
        }
        rules.push(json!({"argv": argv[1..], "args": args}));
    }
    rules
}

/// Write the guard extension; a missing or stale file never weakens
/// the posture because `open` regenerates it per launch.
fn write_pi_guard(state_dir: &Path) -> Result<PathBuf> {
    // JSON strings are valid JS literals — the substitution needs no
    // escaping of its own.
    let source = PI_GUARD_HEAD
        .replace("__RULES__", &json!(pi_guard_rules()).to_string())
        .replace(
            "__MASTER_TMP__",
            &json!(crate::master::tmpdir(state_dir).to_string_lossy()).to_string(),
        );
    let path = pi_guard_path(state_dir);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, source)?;
    Ok(path)
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
}

pub struct PiAdapter {
    /// Swapped at `open()` — the real command line carries flags that
    /// only exist once the agent row is read.
    transport: RwLock<Arc<StdioAdapter>>,
    shared: Arc<Shared>,
    log_path: PathBuf,
    state_dir: PathBuf,
    env: ProviderEnv,
}

impl PiAdapter {
    pub fn new(hooks: AdapterHooks, log_path: &Path, env: &ProviderEnv) -> Self {
        let shared = Arc::new(Shared {
            hooks,
            pending: Pending::new(),
            outcomes: Mutex::new(VecDeque::new()),
            outcome_cv: Condvar::new(),
            turn: Mutex::new(TurnAcc::default()),
            generation: Mutex::new(String::new()),
            interrupt_at: Mutex::new(None),
            active_turn: Mutex::new(None),
            last_activity: Mutex::new(Instant::now()),
            idle_window: Mutex::new(DEFAULT_TURN_IDLE),
            max_turn: Mutex::new(None),
            transport: Mutex::new(None),
            dead: AtomicBool::new(false),
        });
        let routed = Arc::clone(&shared);
        let disconnected = Arc::clone(&shared);
        Self {
            // Bound to the empty generation — replaced by `open`'s mint.
            transport: RwLock::new(StdioAdapter::new_lines(
                &pi_command(env),
                pi_env_scrub(),
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
        }
    }

    /// The master's confinement, appended to its provider log so an
    /// operator can see why a path is unreadable (same as Claude).
    fn log_confinement(&self, policy: &crate::confine::Policy) -> Result<()> {
        if let Some(dir) = self.log_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;
        std::io::Write::write_all(
            &mut log,
            format!(
                "cadence: master confinement: {}\n",
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
            pi_env_scrub()
        };
        StdioAdapter::new_lines(
            command,
            scrub,
            Box::new(move |incoming| routed.dispatch_for(&gen_dispatch, incoming)),
            Box::new(move || disconnected.disconnect_for(&gen_disconnect)),
        )
    }

    /// One `{"id":…,"type":<command>,…}` frame → its `response`.
    /// `success:false` answers with the provider's `error` text — a
    /// definitive reply, never `OutcomeUnknown`. A timeout is unknown:
    /// the command may have been delivered.
    fn request(&self, command: &str, fields: Value) -> Result<Value> {
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
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(resp) => Ok(resp),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shared.pending.map.lock().unwrap().remove(&id);
                Err(Error::unknown(format!(
                    "No response to '{command}' within {}s; provider state is unknown",
                    REQUEST_TIMEOUT.as_secs()
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
        self.emit(
            "cadence/tool_result",
            &json!({
                "summary": crate::store::tool_result_summary(content),
                "is_error": event.get("isError").and_then(Value::as_bool).unwrap_or(false),
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
        let master = crate::master::is_master(&agent.alias);
        if master {
            write_pi_guard(&self.state_dir)?;
        }
        let (command, confinement) = launch_command(&self.env, &self.state_dir, agent);
        if let Some(policy) = &confinement {
            self.log_confinement(policy)?;
        }
        let generation = Uuid::new_v4().simple().to_string()[..12].to_string();
        *self.shared.generation.lock().unwrap() = generation.clone();
        self.shared.dead.store(false, Ordering::SeqCst);
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
            // No update checks or network on the master's startup path.
            env.push(("PI_OFFLINE".to_string(), "1".to_string()));
        }
        let params = agent.params.clone().unwrap_or(Value::Null);
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
        let pid = transport.launch(&agent.cwd, &self.log_path, &env)?;
        *self.transport.write().unwrap() = transport;
        // Effort: validate against the model's real levels, then verify
        // what stuck — Pi answers success even on a silent fallback.
        if let Some(effort) = params.get("effort").and_then(Value::as_str) {
            let applied = self
                .request("set_thinking_level", json!({"level": effort}))
                .and_then(|r| {
                    if r.get("success").and_then(Value::as_bool) == Some(true) {
                        Ok(())
                    } else {
                        Err(command_error("set_thinking_level", &r))
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
        let state = self.request("get_state", json!({}))?;
        if state.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(command_error("get_state", &state));
        }
        let data = state.get("data").cloned().unwrap_or(Value::Null);
        let session_id = data
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let model = data
            .get("model")
            .and_then(|m| m.get("id").or_else(|| m.get("name")))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| agent.model.clone());
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
    }

    /// One durable message → one `prompt` command → `agent_settled`.
    /// The response gates acceptance; the settle carries the outcome.
    fn run_turn(
        &self,
        prompt: &str,
        _client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        let generation = self.shared.generation.lock().unwrap().clone();
        let turn_id = registry::PI_MANAGED_TURN_TOKENS.mint(&generation);
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
        let resp = self.request("prompt", json!({"message": prompt}))?;
        if resp.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(command_error("prompt", &resp));
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
        let acc = waited?;
        let interrupted_here = self.shared.interrupt_at.lock().unwrap().take().is_some();
        let aborted = acc.stop.as_deref() == Some("aborted");
        let (status, error) = if aborted || (interrupted_here && acc.error.is_some()) {
            ("interrupted", None)
        } else if let Some(err) = acc.error {
            ("failed", Some(err))
        } else if acc.stop.as_deref() == Some("error") {
            (
                "failed",
                Some("pi turn ended with stopReason 'error'".to_string()),
            )
        } else {
            ("completed", None)
        };
        Ok(TurnResult {
            turn_id,
            status: status.to_string(),
            text: acc.text.unwrap_or_default(),
            stop_reason: acc.stop,
            error,
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
        transport.wait_exit(Duration::from_secs(3));
        transport.close();
        self.shared.on_disconnect();
    }
}
