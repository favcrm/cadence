//! CAD-615: the master asks the operator for permission.
//!
//! A refused, well-formed command becomes a request. The operator
//! allows it once, saves an allow or deny rule, or rejects it. A
//! single-use grant is bound to the exact argv and cwd, expires, and
//! is consumed by one use. Rules live in the operator-owned
//! `agents/master/permissions.yaml`. The never-list (merge, rollout,
//! secrets, audit approve, the master's own files, the state dir) is
//! absolute: not requestable, not approvable, not rule-able.
//!
//! Callers are checked by the daemon, not here. This module is the
//! record and the match.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::master::CLAUDE_ALLOWED_TOOLS;

/// How long a request stays decidable, and how long an allow-once
/// grant stays usable after the operator approves it.
pub const TTL_SECS: i64 = 15 * 60;

/// The operator-owned rules file, under `agents/master/`.
pub const PERMISSIONS_FILE: &str = "permissions.yaml";

const ARG_MAX: usize = 64;
const ARG_LEN: usize = 512;
const CMD_MAX: usize = 4096;
const REASON_MAX: usize = 500;
const DOC_FILE: &str = "master-permissions.json";

/// Read-only programs the operator may approve inside a registered
/// project checkout. Anything else that is not `cadence` is not
/// requestable.
const READONLY_TOOLS: &[&str] = &["ls", "cat", "grep", "find"];

/// One process-wide lock per state dir. `flock` is per-process, so
/// two daemon threads would not exclude each other; this mutex does.
fn dir_lock(state_dir: &Path) -> Arc<Mutex<()>> {
    static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);
    let mut map = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    let map = map.get_or_insert_with(HashMap::new);
    map.entry(state_dir.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Risk shown on the Needs-you row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Medium,
    High,
}

impl Risk {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// Why a command cannot become a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Class {
    /// Already on the static allowlist — nothing to ask.
    Allowlisted,
    /// The operator may approve it.
    Requestable { risk: Risk },
    /// Absolute refusal. No grant and no rule can cover it.
    Never { why: &'static str },
    /// Pipes, chaining, quotes, redirects, or a shape we will not parse.
    IllFormed { why: &'static str },
}

/// Where an always-allow / don't-ask-again rule applies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The exact argv and cwd.
    Exact,
    /// A fixed verb (`head`) plus trailing argument patterns (`tail`).
    Prefix,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    Allow,
    Deny,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub reason: String,
    pub requested_by: String,
    pub created: i64,
    pub expires_at: i64,
    /// `pending`, `allowed`, `rejected`, `expired`.
    pub status: String,
    pub risk: Risk,
    /// How the operator decided, once it is no longer pending:
    /// `allow_once`, `always`, or `reject`. Empty while pending.
    #[serde(default)]
    pub decision: String,
    /// The line the decision card shows after the operator acts.
    #[serde(default)]
    pub decision_label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Grant {
    id: String,
    request_id: String,
    argv: Vec<String>,
    cwd: String,
    /// 1 until consumed.
    uses_left: u32,
    expires_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub effect: Effect,
    pub scope: Scope,
    /// Exact argv, or the literal verb head of a prefix rule.
    pub argv: Vec<String>,
    /// Prefix-rule argument patterns. Empty for an exact rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
    pub cwd: String,
    pub by: String,
    pub at: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    requests: Vec<Request>,
    #[serde(default)]
    grants: Vec<Grant>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct RulesFile {
    #[serde(default)]
    rules: Vec<Rule>,
}

fn doc_path(state_dir: &Path) -> PathBuf {
    state_dir.join(DOC_FILE)
}

pub(crate) fn alias_ok(alias: &str) -> Result<()> {
    if !alias.is_empty()
        && alias.len() <= 64
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(Error::rejected(
            "a permissions file is agents/<alias>/permissions.yaml — the alias is a short slug",
        ))
    }
}

/// `agents/<alias>/permissions.yaml`. The schema is per agent so a later
/// worker ticket can reuse it. This ticket only loads the master's file.
pub fn rules_file(pm_dir: &Path, alias: &str) -> Result<PathBuf> {
    alias_ok(alias)?;
    Ok(pm_dir.join("agents").join(alias).join(PERMISSIONS_FILE))
}

fn load_doc(state_dir: &Path) -> Result<Doc> {
    let path = doc_path(state_dir);
    if !path.exists() {
        return Ok(Doc::default());
    }
    let text = fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "master permission record is unreadable ({e}) — refusing to guess"
        ))
    })
}

fn save_doc(state_dir: &Path, doc: &Doc) -> Result<()> {
    let path = doc_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(doc)?)?;
    fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

fn reject_rules_symlinks(pm_dir: &Path, alias: &str, path: &Path, action: &str) -> Result<()> {
    let checks = [
        pm_dir.join("agents"),
        pm_dir.join("agents").join(alias),
        path.to_path_buf(),
        path.with_extension("yaml.tmp"),
    ];
    if checks
        .iter()
        .any(|p| p.symlink_metadata().is_ok_and(|m| m.is_symlink()))
    {
        return Err(Error::rejected(format!(
            "agents/{alias}/permissions.yaml is or traverses a symlink — refusing to {action} it"
        )));
    }
    Ok(())
}

fn load_rules(pm_dir: &Path, alias: &str) -> Result<Vec<Rule>> {
    let path = rules_file(pm_dir, alias)?;
    reject_rules_symlinks(pm_dir, alias, &path, "read")?;
    if !path.exists() && path.symlink_metadata().is_err() {
        return Ok(vec![]);
    }
    let text = fs::read_to_string(&path)?;
    let file: RulesFile = serde_yaml::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "agents/{alias}/permissions.yaml is unreadable ({e}) — refusing to guess"
        ))
    })?;
    Ok(file.rules)
}

fn arg_ok(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= ARG_LEN
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '=' | ':' | '-'))
}

fn pattern_arg_ok(arg: &str) -> bool {
    !arg.is_empty()
        && arg.len() <= ARG_LEN
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '/' | '=' | ':' | '-' | '+' | '@' | ',' | '%')
        })
}

/// The command is one plain argv: no shell metacharacters. The same
/// charset the Pi guard uses, applied per argument.
pub fn well_formed(argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv.len() > ARG_MAX {
        return Err(Error::rejected(
            "one plain command, 1-64 arguments — no pipes, chaining or quotes",
        ));
    }
    let joined = argv.join(" ");
    if joined.len() > CMD_MAX || !argv.iter().all(|a| arg_ok(a)) {
        return Err(Error::rejected(
            "no pipes, chaining (`;`, `&&`, `||`), redirects, `$(…)`, backticks, quotes or \
             escapes — run one plain command",
        ));
    }
    Ok(())
}

fn tool_name(argv0: &str) -> &str {
    Path::new(argv0)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(argv0)
}

fn is_cadence(argv0: &str) -> bool {
    tool_name(argv0) == "cadence"
}

fn is_readonly(argv0: &str) -> bool {
    READONLY_TOOLS.contains(&tool_name(argv0))
}

/// A bare `ls`/`cat`/`grep`/`find` — not a path to some other program.
pub fn readonly_tool(argv: &[String]) -> bool {
    argv.first()
        .is_some_and(|a| !a.contains('/') && is_readonly(a))
}

/// Does `argv` match one static allowlist entry (`cadence` included).
pub fn allowlisted(argv: &[String]) -> bool {
    if !is_cadence(argv.first().map(String::as_str).unwrap_or("")) {
        return false;
    }
    let rest: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    for tool in CLAUDE_ALLOWED_TOOLS {
        let Some(inner) = tool.strip_prefix("Bash(").and_then(|t| t.strip_suffix(')')) else {
            continue;
        };
        let (stem, args) = match inner.strip_suffix(" *") {
            Some(stem) => (stem, true),
            None if inner.ends_with('*') => continue,
            None => (inner, false),
        };
        let want: Vec<&str> = stem.split_whitespace().skip(1).collect();
        if want.is_empty() {
            continue;
        }
        let prefix_ok = want
            .iter()
            .enumerate()
            .all(|(i, tok)| rest.get(i) == Some(tok));
        let len_ok = if args {
            rest.len() > want.len()
        } else {
            rest.len() == want.len()
        };
        if prefix_ok && len_ok {
            return true;
        }
    }
    false
}

const NEVER_WRAPPERS: &[&str] = &[
    "sudo", "su", "doas", "env", "timeout", "nice", "nohup", "command", "builtin", "exec", "eval",
    "xargs", "stdbuf", "setsid", "taskset", "ionice", "chrt", "strace", "ltrace", "watch", "time",
    "rtk", "busybox", "sh", "bash", "zsh", "dash", "ksh", "fish", "csh", "tcsh",
];

fn cadence_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "--org"
            | "--state-dir"
            | "--project"
            | "--wake-timeout"
            | "--file"
            | "--provider"
            | "--model"
            | "--effort"
            | "--to"
            | "--since"
            | "--grant"
            | "--reason"
            | "--scope"
            | "--arg"
            | "--agent"
            | "--repo"
            | "--issue"
            | "--pr"
            | "--head"
            | "--source"
            | "--action"
            | "--id"
            | "--target"
            | "--channel"
            | "--version"
            | "--config"
            | "--db"
            | "--as"
            | "--timeout"
            | "--until"
            | "--holder"
            | "--ttl"
            | "--digest"
            | "--expected-revision"
            | "--question"
            | "-R"
            | "-C"
    )
}

fn cadence_positionals(argv: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        let a = argv[i].as_str();
        if a == "--" {
            out.extend(argv[i + 1..].iter().map(String::as_str));
            break;
        }
        if cadence_flag_takes_value(a) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        out.push(a);
        i += 1;
    }
    out
}

fn cadence_subcommand(argv: &[String]) -> Option<&str> {
    cadence_positionals(argv).into_iter().next()
}

fn never_why(argv: &[String], cwd: &Path, state_dirs: &[&Path]) -> Option<&'static str> {
    never_why_for(argv, cwd, state_dirs, true)
}

fn never_why_command_slice(
    slice: &[String],
    cwd: &Path,
    state_dirs: &[&Path],
) -> Option<&'static str> {
    let tokens: Vec<&str> = slice.iter().map(|s| tool_name(s)).collect();
    let has = |w: &str| tokens.contains(&w);
    let cadence = is_cadence(slice.first().map(String::as_str).unwrap_or(""));
    if has("merge") && (cadence || matches!(tokens.first(), Some(&"git" | &"gh"))) {
        return Some("merge is never requestable");
    }
    if tokens.first() == Some(&"git")
        && has("push")
        && slice.iter().skip(1).any(|a| {
            a == "-f"
                || a == "--force"
                || a.starts_with("--force=")
                || a.starts_with("--force-with-lease")
                || (a.starts_with('-') && !a.starts_with("--") && a.contains('f'))
                || (a.starts_with('+') && a.len() > 1)
        })
    {
        return Some("git force-push is never requestable");
    }
    if cadence {
        let stripped = strip_own_state_dir(slice, cwd, state_dirs);
        let pos = cadence_positionals(&stripped);
        let noun = pos.first().copied();
        if noun == Some("update") {
            return Some("cadence update is never requestable");
        }
        if noun == Some("rollout") || has("rollout") {
            return Some("rollout is never requestable");
        }
        if noun.is_some_and(|n| n.starts_with("secret"))
            || tokens.iter().any(|t| t.starts_with("secret"))
        {
            return Some("secret commands are never requestable");
        }
        if (noun == Some("daemon") && pos[1..].iter().any(|t| matches!(*t, "restart" | "stop")))
            || (noun == Some("master")
                && pos[1..].iter().any(|t| {
                    matches!(
                        *t,
                        "allow-once" | "always-allow" | "reject" | "revoke-permission" | "resume"
                    )
                }))
        {
            // CAD-1586: `master resume` is lifecycle authority carried by a
            // settings grant, never by an approved argv — a granted child
            // runs it only as the bound agent, so it is never requestable.
            return Some("daemon restart and permission decisions are never requestable");
        }
        if noun == Some("audit")
            && pos[1..]
                .iter()
                .any(|t| matches!(*t, "approve" | "accept-revise" | "designate"))
        {
            // CAD-918: a granted child runs with operator authority, so
            // neither an approval nor a designation can be requested.
            return Some("audit approve and audit designate are never requestable");
        }
        if noun == Some("settings") || has("settings") {
            // CAD-1586: reads stay requestable — an approved child may read
            // settings (argv approval is enough for a read). Mutations, the
            // catalog fetch and every grant verb are not: argv never binds
            // payload bytes, and a granted child can never mint settings
            // delegation.
            const NEVER_SETTINGS: &[&str] = &[
                "apply", "put", "remove", "rename", "scope", "set", "clear", "import", "sync",
                "grant", "refresh",
            ];
            if tokens.iter().any(|t| NEVER_SETTINGS.contains(t))
                || pos.iter().any(|t| NEVER_SETTINGS.contains(t))
            {
                return Some(
                    "settings mutations, the catalog fetch and grant verbs are never requestable",
                );
            }
        }
        if noun == Some("master")
            && pos[1..].contains(&"edit")
            && pos[1..].iter().any(|t| protected_name(tool_name(t)))
        {
            return Some(
                "agent SOUL.md, AGENT.md and permissions.yaml files are never requestable",
            );
        }
    }
    None
}

fn never_why_for(
    argv: &[String],
    cwd: &Path,
    state_dirs: &[&Path],
    allow_master_scratch: bool,
) -> Option<&'static str> {
    let is_non_bash_tool = argv
        .first()
        .is_some_and(|t| worker_allowed_non_bash_tool(t));
    let tokens: Vec<&str> = argv.iter().map(|s| tool_name(s)).collect();
    if !allow_master_scratch && !is_non_bash_tool {
        if argv.is_empty() || argv.len() > ARG_MAX || !argv.iter().all(|a| pattern_arg_ok(a)) {
            return Some("malformed shell command is never requestable");
        }
        if tokens.first().is_some_and(|t| NEVER_WRAPPERS.contains(t)) {
            return Some("wrapper and subshell commands are never requestable");
        }
        if tokens.first() == Some(&"find")
            && argv.iter().skip(1).any(|a| {
                matches!(
                    a.as_str(),
                    "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete"
                )
            })
        {
            return Some("find -exec and -delete are never requestable");
        }
    }
    if let Some(why) = never_why_command_slice(argv, cwd, state_dirs) {
        return Some(why);
    }
    if !allow_master_scratch && !is_non_bash_tool {
        for idx in 1..argv.len() {
            if is_cadence(&argv[idx]) || matches!(tokens[idx], "git" | "gh") {
                if let Some(why) = never_why_command_slice(&argv[idx..], cwd, state_dirs) {
                    return Some(why);
                }
            }
        }
    }
    if argv.iter().any(|a| touches_protected_file(a, cwd)) {
        return Some("agent SOUL.md, AGENT.md and permissions.yaml files are never requestable");
    }
    if touches_state_for(argv, cwd, state_dirs, allow_master_scratch) {
        return Some("the daemon state dir is never requestable");
    }
    None
}

/// Drop `.` and `..` so `agents/master/foo/../SOUL.md` is the same
/// file as `agents/master/SOUL.md`. The file does not have to exist.
fn lexical_clean(arg: &str) -> String {
    let flat = arg.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for seg in flat.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn protected_name(name: &str) -> bool {
    matches!(name, "SOUL.md" | "AGENT.md" | "permissions.yaml")
}

fn protected_file(arg: &str) -> bool {
    let path = lexical_clean(arg);
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    segs.len() >= 3
        && segs[segs.len() - 3] == "agents"
        && !segs[segs.len() - 2].is_empty()
        && protected_name(segs[segs.len() - 1])
}

fn state_root(dir: &Path) -> PathBuf {
    fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

fn arg_path_candidates(arg: &str) -> [Option<&str>; 2] {
    let rhs = arg
        .split_once('=')
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty());
    [Some(arg), rhs]
}

fn has_tilde_path(cand: &str) -> bool {
    if cand.starts_with('~') {
        return true;
    }
    !cand.contains("://") && cand.split(':').skip(1).any(|part| part.starts_with('~'))
}

fn expand_path_symlinks(path: &Path) -> std::result::Result<PathBuf, ()> {
    if path.to_string_lossy().starts_with('~') {
        return Err(());
    }
    const MAX_SYMLINK_HOPS: usize = 40;
    let mut cur = PathBuf::new();
    let mut queue: VecDeque<OsString> = VecDeque::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => cur.push(p.as_os_str()),
            Component::RootDir => cur.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir | Component::Normal(_) => {
                queue.push_back(comp.as_os_str().to_os_string());
            }
        }
    }
    let mut hops = 0usize;
    while let Some(part) = queue.pop_front() {
        if part.as_os_str() == std::ffi::OsStr::new("..") {
            if matches!(cur.components().next_back(), Some(Component::Normal(_))) {
                cur.pop();
            } else if !cur.has_root() {
                cur.push("..");
            }
            continue;
        }
        let next = cur.join(&part);
        if next
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            hops += 1;
            if hops > MAX_SYMLINK_HOPS {
                return Err(());
            }
            let target = fs::read_link(&next).map_err(|_| ())?;
            let mut suffix = Vec::new();
            for comp in target.components() {
                match comp {
                    Component::Prefix(p) => {
                        cur = PathBuf::new();
                        cur.push(p.as_os_str());
                    }
                    Component::RootDir => {
                        if !matches!(cur.components().next(), Some(Component::Prefix(_))) {
                            cur = PathBuf::new();
                        }
                        cur.push(Path::new("/"));
                    }
                    Component::CurDir => {}
                    Component::ParentDir | Component::Normal(_) => {
                        suffix.push(comp.as_os_str().to_os_string());
                    }
                }
            }
            for item in suffix.into_iter().rev() {
                queue.push_front(item);
            }
            continue;
        }
        cur = next;
    }
    Ok(cur)
}

fn touches_protected_file(arg: &str, cwd: &Path) -> bool {
    arg_path_candidates(arg).into_iter().flatten().any(|cand| {
        if has_tilde_path(cand) || protected_file(cand) {
            return true;
        }
        let probed = probe_path(cand, cwd);
        if protected_file(&probed.to_string_lossy()) {
            return true;
        }
        match expand_path_symlinks(&probed) {
            Ok(exp) => protected_file(&exp.to_string_lossy()),
            Err(()) => true,
        }
    })
}

pub(crate) fn touches_pm_agents_dir(arg: &str, cwd: &Path, pm_dir: &Path) -> bool {
    touches_pm_agents_dir_inner(arg, cwd, pm_dir, false)
}

pub(crate) fn argv_touches_pm_agents_dir(argv: &[String], cwd: &Path, pm_dir: &Path) -> bool {
    let is_wiki = is_cadence(argv.first().map(String::as_str).unwrap_or(""))
        && cadence_subcommand(argv) == Some("wiki");
    if !is_wiki {
        let Ok(agents_root) = expand_for_state(&pm_dir.join("agents")) else {
            return true;
        };
        match expand_for_state(cwd) {
            Ok(exp) if exp == agents_root || exp.starts_with(&agents_root) => return true,
            Ok(_) => {}
            Err(()) => return true,
        }
    }
    argv.iter()
        .skip(1)
        .any(|a| touches_pm_agents_dir_inner(a, cwd, pm_dir, is_wiki))
}

fn touches_pm_agents_dir_inner(
    arg: &str,
    cwd: &Path,
    pm_dir: &Path,
    allow_wiki_knowledge: bool,
) -> bool {
    let Ok(agents_root) = expand_for_state(&pm_dir.join("agents")) else {
        return true;
    };
    arg_path_candidates(arg).into_iter().flatten().any(|cand| {
        if has_tilde_path(cand) {
            return true;
        }
        let probed = probe_path(cand, cwd);
        match expand_for_state(&probed) {
            Ok(exp) => {
                if exp == agents_root || exp.starts_with(&agents_root) {
                    if allow_wiki_knowledge {
                        if let Ok(rel) = exp.strip_prefix(&agents_root) {
                            let mut comps = rel.components();
                            if matches!(comps.next(), Some(Component::Normal(_)))
                                && matches!(comps.next(), Some(Component::Normal(c)) if c == "knowledge")
                                && comps.next().is_some()
                            {
                                return false;
                            }
                        }
                    }
                    true
                } else {
                    false
                }
            }
            Err(()) => true,
        }
    })
}

fn expand_for_state(path: &Path) -> std::result::Result<PathBuf, ()> {
    fs::canonicalize(path)
        .map_err(|_| ())
        .or_else(|()| expand_path_symlinks(path))
}

fn in_state_dirs(canon: &Path, state_dirs: &[&Path]) -> bool {
    state_dirs.iter().any(|dir| {
        let root = state_root(dir);
        canon == root || canon.starts_with(&root)
    })
}

fn is_state_root(arg: &str, cwd: &Path, state_dirs: &[&Path]) -> bool {
    let Ok(canon) = expand_for_state(&probe_path(arg, cwd)) else {
        return false;
    };
    state_dirs.iter().any(|dir| canon == state_root(dir))
}

/// The master's own scratch (`state/master/tmp` and `state/master/cwd`)
/// is where its plan files and shell cwd live. Other state paths stay
/// never-requestable.
fn under_master_scratch(canon: &Path, state_dirs: &[&Path]) -> bool {
    state_dirs.iter().any(|dir| {
        let root = state_root(dir);
        ["tmp", "cwd"].iter().any(|name| {
            let base = root.join("master").join(name);
            canon == base || canon.starts_with(&base)
        })
    })
}

/// Drop `--state-dir <this daemon>` so an allowlisted verb still matches
/// when the master tool line names the daemon's own state dir.
fn strip_own_state_dir(argv: &[String], cwd: &Path, state_dirs: &[&Path]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "--state-dir" {
            if i + 1 < argv.len() && is_state_root(&argv[i + 1], cwd, state_dirs) {
                i += 2;
                continue;
            }
        } else if let Some(val) = arg.strip_prefix("--state-dir=") {
            if is_state_root(val, cwd, state_dirs) {
                i += 1;
                continue;
            }
        }
        out.push(arg.clone());
        i += 1;
    }
    out
}

fn touches_state_path(
    arg: &str,
    cwd: &Path,
    state_dirs: &[&Path],
    allow_master_scratch: bool,
) -> bool {
    if !allow_master_scratch {
        if !cwd.is_absolute() {
            return true;
        }
        match expand_for_state(cwd) {
            Ok(canon_cwd) if !in_state_dirs(&canon_cwd, state_dirs) => {}
            _ => return true,
        }
    }
    arg_path_candidates(arg).into_iter().flatten().any(|cand| {
        if has_tilde_path(cand)
            || cand.contains(".local/state/cadence")
            || cand.contains(".local/share/cadence")
        {
            return true;
        }
        let Ok(canon) = expand_for_state(&probe_path(cand, cwd)) else {
            return true;
        };
        if allow_master_scratch && under_master_scratch(&canon, state_dirs) {
            return false;
        }
        in_state_dirs(&canon, state_dirs)
    })
}

fn touches_state_for(
    argv: &[String],
    cwd: &Path,
    state_dirs: &[&Path],
    allow_master_scratch: bool,
) -> bool {
    let Ok(canon_cwd) = expand_for_state(cwd) else {
        return true;
    };
    let cwd_in_state = in_state_dirs(&canon_cwd, state_dirs);
    if !allow_master_scratch && (!cwd.is_absolute() || cwd_in_state) {
        return true;
    }
    // The master's cwd is inside the state dir (`master/cwd`). That
    // fact is not a path the command touches — only arguments that
    // resolve into a state dir are. This daemon's own `--state-dir`
    // value, and files under `state/master/tmp` or `state/master/cwd`
    // (for the master only), are the master's tool line, not a reach into the state dir.
    let cwd_is_root = state_dirs.iter().any(|dir| canon_cwd == state_root(dir));
    let mut probes = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "--state-dir" {
            if i + 1 < argv.len() {
                let val = &argv[i + 1];
                if !allow_master_scratch || !is_state_root(val, &canon_cwd, state_dirs) {
                    probes.push(probe_path(val, &canon_cwd));
                }
                i += 2;
                continue;
            }
        } else if let Some(val) = arg.strip_prefix("--state-dir=") {
            if !allow_master_scratch || !is_state_root(val, &canon_cwd, state_dirs) {
                probes.push(probe_path(val, &canon_cwd));
            }
            i += 1;
            continue;
        }
        for cand in arg_path_candidates(arg).into_iter().flatten() {
            if has_tilde_path(cand)
                || cand.contains(".local/state/cadence")
                || cand.contains(".local/share/cadence")
            {
                return true;
            }
            if !allow_master_scratch
                || cand.starts_with('/')
                || cand.starts_with('.')
                || (!cwd_is_root && cand.split(['/', '\\']).any(|seg| seg == ".."))
            {
                probes.push(probe_path(cand, &canon_cwd));
            }
        }
        i += 1;
    }
    for probe in probes {
        let Ok(canon) = expand_for_state(&probe) else {
            return true;
        };
        if allow_master_scratch && under_master_scratch(&canon, state_dirs) {
            continue;
        }
        if in_state_dirs(&canon, state_dirs) {
            return true;
        }
    }
    false
}

fn probe_path(arg: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(arg);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn risk_of(argv: &[String]) -> Risk {
    if is_readonly(argv.first().map(String::as_str).unwrap_or("")) {
        return Risk::Low;
    }
    const HIGH: &[&str] = &[
        "new", "put", "write", "edit", "rm", "delete", "set", "dispatch", "send", "remove",
    ];
    if argv.iter().any(|a| HIGH.contains(&a.as_str())) {
        Risk::High
    } else {
        Risk::Medium
    }
}

/// Classify `argv` run in `cwd`. `checkouts` are registered project
/// roots (canonical). `state_dirs` are the daemon's state dir and the
/// production state dir — both untouchable.
fn normalize_argv(argv: &[String]) -> Vec<String> {
    let mut out = argv.to_vec();
    if let Some(first) = out.first_mut() {
        let name = tool_name(first).to_string();
        if name == "cadence" || READONLY_TOOLS.contains(&name.as_str()) {
            *first = name;
        }
    }
    out
}

pub fn classify(argv: &[String], cwd: &Path, checkouts: &[PathBuf], state_dirs: &[&Path]) -> Class {
    let argv = normalize_argv(argv);
    let argv = argv.as_slice();
    if let Err(e) = well_formed(argv) {
        return Class::IllFormed {
            why: if e.to_string().contains("1-64") {
                "one plain command"
            } else {
                "no pipes, chaining, redirects, quotes or escapes"
            },
        };
    }
    if let Some(why) = never_why(argv, cwd, state_dirs) {
        return Class::Never { why };
    }
    if allowlisted(&strip_own_state_dir(argv, cwd, state_dirs)) {
        return Class::Allowlisted;
    }
    if is_cadence(argv.first().map(String::as_str).unwrap_or("")) {
        return Class::Requestable {
            risk: risk_of(argv),
        };
    }
    if is_readonly(argv.first().map(String::as_str).unwrap_or(""))
        && paths_in_checkouts(argv, cwd, checkouts)
    {
        return Class::Requestable { risk: Risk::Low };
    }
    Class::Never {
        why: "only a cadence verb, or ls/cat/grep/find inside a registered project checkout, can be requested",
    }
}

fn paths_in_checkouts(argv: &[String], cwd: &Path, checkouts: &[PathBuf]) -> bool {
    if checkouts.is_empty() {
        return false;
    }
    let tool = tool_name(argv.first().map(String::as_str).unwrap_or(""));
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    let paths: Vec<&str> = match tool {
        "grep" => {
            // First non-flag is the pattern; every later non-flag is a
            // file and at least one file is required.
            let mut rest = args.iter().copied().filter(|a| !a.starts_with('-'));
            if rest.next().is_none() {
                return false;
            }
            let files: Vec<&str> = rest.collect();
            if files.is_empty() {
                return false;
            }
            files
        }
        _ => {
            let paths: Vec<&str> = args.into_iter().filter(|a| !a.starts_with('-')).collect();
            if paths.is_empty() {
                return false;
            }
            paths
        }
    };
    paths.into_iter().all(|p| {
        let abs = if Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            cwd.join(p)
        };
        let Ok(canon) = fs::canonicalize(&abs) else {
            return false;
        };
        checkouts
            .iter()
            .any(|root| canon == *root || canon.starts_with(root))
    })
}

fn canonical_cwd(cwd: &Path) -> Result<PathBuf> {
    fs::canonicalize(cwd).map_err(|_| {
        Error::rejected(format!(
            "cwd {} does not exist — a request records a real directory",
            cwd.display()
        ))
    })
}

fn fresh_id(prefix: &str) -> String {
    let n = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &n[..12])
}

fn expire(doc: &mut Doc, now: i64) {
    for req in &mut doc.requests {
        if req.status == "pending" && req.expires_at <= now {
            req.status = "expired".into();
        }
    }
    doc.grants.retain(|g| g.expires_at > now && g.uses_left > 0);
}

fn with_doc<T>(state_dir: &Path, now: i64, f: impl FnOnce(&mut Doc) -> Result<T>) -> Result<T> {
    let lock = dir_lock(state_dir);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = load_doc(state_dir)?;
    expire(&mut doc, now);
    let out = f(&mut doc)?;
    save_doc(state_dir, &doc)?;
    Ok(out)
}

/// A pending request for this exact argv and cwd, if one is still live.
#[allow(clippy::too_many_arguments)]
pub fn ask(
    state_dir: &Path,
    argv: &[String],
    cwd: &Path,
    reason: &str,
    requested_by: &str,
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
) -> Result<Request> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > REASON_MAX {
        return Err(Error::rejected(format!(
            "a reason is 1-{REASON_MAX} characters"
        )));
    }
    if requested_by != crate::master::ALIAS {
        return Err(Error::rejected(
            "only the master requests permission — this caller is not the master",
        ));
    }
    let cwd = canonical_cwd(cwd)?;
    let argv = normalize_argv(argv);
    let argv = argv.as_slice();
    match classify(argv, &cwd, checkouts, state_dirs) {
        Class::Allowlisted => Err(Error::rejected(
            "that command is already allowlisted — run it, do not ask",
        )),
        Class::Never { why } => Err(Error::rejected(format!(
            "{why} — it cannot be approved and no permission rule can cover it"
        ))),
        Class::IllFormed { why } => Err(Error::rejected(why)),
        Class::Requestable { risk } => {
            let cwd_s = cwd.to_string_lossy().into_owned();
            with_doc(state_dir, now, |doc| {
                if let Some(existing) = doc
                    .requests
                    .iter()
                    .find(|r| r.status == "pending" && r.argv == argv && r.cwd == cwd_s)
                {
                    return Ok(existing.clone());
                }
                let req = Request {
                    id: fresh_id("mp"),
                    argv: argv.to_vec(),
                    cwd: cwd_s,
                    reason: reason.to_string(),
                    requested_by: requested_by.to_string(),
                    created: now,
                    expires_at: now + TTL_SECS,
                    status: "pending".into(),
                    risk,
                    decision: String::new(),
                    decision_label: String::new(),
                };
                doc.requests.push(req.clone());
                Ok(req)
            })
        }
    }
}

fn pending<'a>(doc: &'a mut Doc, id: &str, now: i64) -> Result<&'a mut Request> {
    let req = doc
        .requests
        .iter_mut()
        .find(|r| r.id == id)
        .ok_or_else(|| Error::rejected(format!("no permission request '{id}'")))?;
    if req.status != "pending" {
        return Err(Error::rejected(format!(
            "permission request '{id}' is {} — it cannot be decided again",
            req.status
        )));
    }
    if req.expires_at <= now {
        req.status = "expired".into();
        return Err(Error::rejected(format!(
            "permission request '{id}' expired"
        )));
    }
    Ok(req)
}

fn refuse_never(req: &Request, checkouts: &[PathBuf], state_dirs: &[&Path]) -> Result<()> {
    let why = if req.requested_by == crate::master::ALIAS {
        match classify(&req.argv, Path::new(&req.cwd), checkouts, state_dirs) {
            Class::Never { why } => Some(why),
            _ => None,
        }
    } else {
        never_why_for(&req.argv, Path::new(&req.cwd), state_dirs, false)
    };
    if let Some(why) = why {
        return Err(Error::rejected(format!(
            "{why} — it cannot be approved and no permission rule can cover it"
        )));
    }
    Ok(())
}

/// Allow the request's exact argv and cwd once. The never-list is
/// checked again here: a planted pending request is not a grant.
#[allow(clippy::too_many_arguments)]
pub fn allow_once(
    state_dir: &Path,
    id: &str,
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
) -> Result<Request> {
    with_doc(state_dir, now, |doc| {
        let req = pending(doc, id, now)?.clone();
        refuse_never(&req, checkouts, state_dirs)?;
        let stored = doc
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .expect("pending");
        stored.status = "allowed".into();
        stored.decision = "allow_once".into();
        stored.decision_label = format!("Allowed once by operator {}", decision_hm(now));
        let mut req = req;
        req.status = stored.status.clone();
        req.decision = stored.decision.clone();
        req.decision_label = stored.decision_label.clone();
        if req.requested_by == crate::master::ALIAS {
            doc.grants.push(Grant {
                id: fresh_id("mg"),
                request_id: req.id.clone(),
                argv: req.argv.clone(),
                cwd: req.cwd.clone(),
                uses_left: 1,
                expires_at: now + TTL_SECS,
            });
        }
        Ok(req)
    })
}

/// A wildcard is only legal as the last character of an argument, and
/// never in the verb head.
pub fn validate_pattern(pat: &str) -> Result<()> {
    if !pat.contains('*') {
        return if pattern_arg_ok(pat) {
            Ok(())
        } else {
            Err(Error::rejected(format!(
                "pattern '{pat}' has characters a permission rule cannot store"
            )))
        };
    }
    if !pat.ends_with('*') || pat.matches('*').count() != 1 {
        return Err(Error::rejected(format!(
            "wildcard in '{pat}' must be a single '*' at the end of the argument"
        )));
    }
    let stem = &pat[..pat.len() - 1];
    if stem.is_empty() {
        return Err(Error::rejected(
            "a wildcard needs a stem — a bare '*' would match every argument",
        ));
    }
    if pattern_arg_ok(stem) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "pattern '{pat}' has characters a permission rule cannot store"
        )))
    }
}

fn dot_segment(s: &str) -> bool {
    s.split(['/', '\\']).any(|seg| seg == "." || seg == "..")
}

fn pattern_matches(pat: &str, arg: &str) -> bool {
    if dot_segment(arg) || dot_segment(pat) {
        return false;
    }
    match pat.strip_suffix('*') {
        Some(stem) => {
            !stem.is_empty()
                && arg.starts_with(stem)
                && (stem.ends_with('/')
                    || arg.len() == stem.len()
                    || arg[stem.len()..].starts_with('/'))
        }
        None => pat == arg,
    }
}

/// Reject a rule that would name a different verb or a mid-argument
/// wildcard. `head` is the literal verb; `tail` is the argument patterns.
pub fn check_rule_shape(head: &[String], tail: &[String], scope: &Scope) -> Result<()> {
    let min_head = if is_cadence(head.first().map(String::as_str).unwrap_or("")) {
        2
    } else {
        1
    };
    match scope {
        Scope::Exact => {
            if head.len() < min_head || !tail.is_empty() {
                return Err(Error::rejected(
                    "an exact rule is the full command, with no wildcard tail",
                ));
            }
            if head.iter().any(|a| a.contains('*')) {
                return Err(Error::rejected("an exact rule cannot contain a wildcard"));
            }
        }
        Scope::Prefix => {
            if head.len() < min_head {
                return Err(Error::rejected(
                    "a prefix rule keeps the verb — at least `cadence <verb>` — and cannot widen to another verb",
                ));
            }
            if head.iter().any(|a| a.contains('*')) {
                return Err(Error::rejected(
                    "a wildcard in the verb would widen the rule to a different command — refused",
                ));
            }
            if tail.is_empty() {
                return Err(Error::rejected(
                    "a prefix rule needs a narrowed argument pattern — a bare verb is not a rule",
                ));
            }
            for pat in tail {
                validate_pattern(pat)?;
            }
        }
    }
    Ok(())
}

fn rule_matches(rule: &Rule, argv: &[String], cwd: &str) -> bool {
    if rule.cwd != cwd {
        return false;
    }
    match rule.scope {
        Scope::Exact => rule.argv == argv,
        Scope::Prefix => {
            let head_ok =
                rule.argv.len() < argv.len() && argv.iter().zip(&rule.argv).all(|(a, h)| a == h);
            let rest = &argv[rule.argv.len()..];
            head_ok
                && rest.len() == rule.tail.len()
                && rest
                    .iter()
                    .zip(&rule.tail)
                    .all(|(a, p)| pattern_matches(p, a))
        }
    }
}

/// Save an allow rule (exact or prefix) for a pending request. The
/// caller writes the yaml; this returns the rule and marks the request.
/// A never-list command cannot become a rule.
#[allow(clippy::too_many_arguments)]
pub fn always_rule(
    state_dir: &Path,
    id: &str,
    scope: Scope,
    tail: &[String],
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
) -> Result<(Request, Rule)> {
    with_doc(state_dir, now, |doc| {
        let req = pending(doc, id, now)?.clone();
        refuse_never(&req, checkouts, state_dirs)?;
        let (head, tail) = match &scope {
            Scope::Exact => (req.argv.clone(), vec![]),
            Scope::Prefix => {
                if tail.len() >= req.argv.len() {
                    return Err(Error::rejected(
                        "a prefix tail must be narrower than the whole command",
                    ));
                }
                let split = req.argv.len() - tail.len();
                (req.argv[..split].to_vec(), tail.to_vec())
            }
        };
        check_rule_shape(&head, &tail, &scope)?;
        // The rule must match the request it came from — it cannot
        // name a different verb than the one the master asked to run.
        let draft = Rule {
            id: fresh_id("mr"),
            effect: Effect::Allow,
            scope: scope.clone(),
            argv: head,
            tail,
            cwd: req.cwd.clone(),
            by: "operator".into(),
            at: now,
        };
        if !rule_matches(&draft, &req.argv, &req.cwd) {
            return Err(Error::rejected(
                "that rule does not match the requested command — it cannot widen to a different verb",
            ));
        }
        let label = match draft.scope {
            Scope::Exact => format!("Always: `{}`", draft.argv.join(" ")),
            Scope::Prefix => format!(
                "Always: `{} {}`",
                draft.argv.join(" "),
                draft.tail.join(" ")
            ),
        };
        let stored = doc
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .expect("pending");
        stored.status = "allowed".into();
        stored.decision = "always".into();
        stored.decision_label = label.clone();
        let mut req = req;
        req.status = "allowed".into();
        req.decision = "always".into();
        req.decision_label = label;
        Ok((req, draft))
    })
}

/// Reject a pending request. `dont_ask_again` returns a deny rule for
/// the exact command; the caller persists it.
pub fn reject(
    state_dir: &Path,
    id: &str,
    dont_ask_again: bool,
    now: i64,
) -> Result<(Request, Option<Rule>)> {
    with_doc(state_dir, now, |doc| {
        let mut req = pending(doc, id, now)?.clone();
        let label = if dont_ask_again {
            "Rejected — won't ask again".to_string()
        } else {
            "Rejected".to_string()
        };
        let stored = doc
            .requests
            .iter_mut()
            .find(|r| r.id == id)
            .expect("pending");
        stored.status = "rejected".into();
        stored.decision = "reject".into();
        stored.decision_label = label.clone();
        req.status = "rejected".into();
        req.decision = "reject".into();
        req.decision_label = label;
        let rule = dont_ask_again.then(|| Rule {
            id: fresh_id("mr"),
            effect: Effect::Deny,
            scope: Scope::Exact,
            argv: req.argv.clone(),
            tail: vec![],
            cwd: req.cwd.clone(),
            by: "operator".into(),
            at: now,
        });
        Ok((req, rule))
    })
}

/// Put an allowed or rejected request back to pending. Used when the
/// operator's git commit of the rule fails, so the Needs-you row returns.
pub fn reopen(state_dir: &Path, id: &str, now: i64) -> Result<()> {
    with_doc(state_dir, now, |doc| {
        if let Some(req) = doc.requests.iter_mut().find(|r| r.id == id) {
            if req.status == "allowed" || req.status == "rejected" {
                req.status = "pending".into();
                req.decision.clear();
                req.decision_label.clear();
            }
        }
        Ok(())
    })
}

pub fn read_rules(pm_dir: &Path, alias: &str) -> Result<Vec<Rule>> {
    load_rules(pm_dir, alias)
}

/// Rules that still match. A hand-edited file can carry an empty stem,
/// a mid-argument `*`, or a never-list command; those rules are dropped
/// here so they cannot allow anything. `read_rules` still returns them
/// so the operator can revoke the row.
fn live_rules(pm_dir: &Path, alias: &str, state_dirs: &[&Path]) -> Result<Vec<Rule>> {
    let rules = load_rules(pm_dir, alias)?;
    Ok(rules
        .into_iter()
        .filter(|rule| rule_live_for(alias, rule, state_dirs, pm_dir))
        .collect())
}

fn rule_live_for(alias: &str, rule: &Rule, state_dirs: &[&Path], pm_dir: &Path) -> bool {
    if check_rule_shape(&rule.argv, &rule.tail, &rule.scope).is_err() {
        return false;
    }
    let mut probe = rule.argv.clone();
    for pat in &rule.tail {
        if pat.contains('*') {
            let Some(stem) = pat.strip_suffix('*').filter(|s| !s.is_empty()) else {
                return false;
            };
            probe.push(stem.to_string());
        } else {
            probe.push(pat.clone());
        }
    }
    let cwd = Path::new(&rule.cwd);
    if alias == crate::master::ALIAS {
        !matches!(classify(&probe, cwd, &[], state_dirs), Class::Never { .. })
    } else {
        never_why_for(&probe, cwd, state_dirs, false).is_none()
            && !argv_touches_pm_agents_dir(&probe, cwd, pm_dir)
    }
}

pub fn write_rules(pm_dir: &Path, alias: &str, rules: &[Rule]) -> Result<PathBuf> {
    let path = rules_file(pm_dir, alias)?;
    reject_rules_symlinks(pm_dir, alias, &path, "write")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let file = RulesFile {
        rules: rules.to_vec(),
    };
    let text = serde_yaml::to_string(&file).map_err(|e| Error::internal(e.to_string()))?;
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(path)
}

/// Drop a rule by id. Returns the removed rule.
pub fn revoke_rule(pm_dir: &Path, alias: &str, id: &str) -> Result<Rule> {
    let mut rules = load_rules(pm_dir, alias)?;
    let pos = rules
        .iter()
        .position(|r| r.id == id)
        .ok_or_else(|| Error::rejected(format!("no permission rule '{id}'")))?;
    let rule = rules.remove(pos);
    write_rules(pm_dir, alias, &rules)?;
    Ok(rule)
}

fn known_rule_aliases(pm_dir: &Path) -> Vec<String> {
    let mut aliases = vec![crate::master::ALIAS.to_string()];
    let agents_dir = pm_dir.join("agents");
    if let Ok(entries) = fs::read_dir(agents_dir) {
        let mut extra: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|name| name != crate::master::ALIAS && alias_ok(name).is_ok())
            .collect();
        extra.sort();
        aliases.extend(extra);
    }
    aliases
}

/// Read all saved permission rules across `master` and registered workers,
/// tagged by agent alias. An unreadable worker file is skipped here so one
/// corrupt file does not blank Settings; worker evaluation itself still fails closed.
pub fn list_all_rules(pm_dir: &Path) -> Vec<(String, Rule)> {
    let mut out = Vec::new();
    for alias in known_rule_aliases(pm_dir) {
        if let Ok(rules) = read_rules(pm_dir, &alias) {
            for rule in rules {
                out.push((alias.clone(), rule));
            }
        }
    }
    out
}

/// Locate the agent alias whose `permissions.yaml` owns `rule_id`.
pub fn find_rule_owner(pm_dir: &Path, rule_id: &str) -> Result<String> {
    let mut first_err = None;
    for alias in known_rule_aliases(pm_dir) {
        match load_rules(pm_dir, &alias) {
            Ok(rules) => {
                if rules.iter().any(|r| r.id == rule_id) {
                    return Ok(alias);
                }
            }
            Err(e) if first_err.is_none() => first_err = Some(e),
            Err(_) => {}
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }
    Err(Error::rejected(format!("no permission rule '{rule_id}'")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Use {
    /// A single-use grant was consumed.
    Grant { request_id: String },
    /// An allow rule matched. It is not consumed.
    Rule { id: String },
}

/// Peek: a live single-use grant or an allow rule matches, and no deny
/// rule matches. Does not consume.
pub fn peek(
    state_dir: &Path,
    pm_dir: Option<&Path>,
    argv: &[String],
    cwd: &Path,
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
) -> Result<bool> {
    Ok(decide_use(
        state_dir, pm_dir, argv, cwd, checkouts, state_dirs, now, false,
    )?
    .is_some())
}

/// Consume a single-use grant, or match an allow rule. A deny rule
/// wins. The never-list still refuses, even when a grant was planted
/// in the record.
pub fn take(
    state_dir: &Path,
    pm_dir: Option<&Path>,
    argv: &[String],
    cwd: &Path,
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
) -> Result<Use> {
    decide_use(
        state_dir, pm_dir, argv, cwd, checkouts, state_dirs, now, true,
    )?
    .ok_or_else(|| Error::rejected("no live permission for that exact command and directory"))
}

#[allow(clippy::too_many_arguments)]
fn decide_use(
    state_dir: &Path,
    pm_dir: Option<&Path>,
    argv: &[String],
    cwd: &Path,
    checkouts: &[PathBuf],
    state_dirs: &[&Path],
    now: i64,
    consume: bool,
) -> Result<Option<Use>> {
    let argv_owned = normalize_argv(argv);
    let argv = argv_owned.as_slice();
    let cwd = match canonical_cwd(cwd) {
        Ok(c) => c,
        Err(_) => return Ok(None),
    };
    // Never-list first, before any grant or rule is consulted.
    if let Class::Never { why } = classify(argv, &cwd, checkouts, state_dirs) {
        return Err(Error::rejected(format!(
            "{why} — a grant or a rule cannot allow it"
        )));
    }
    let cwd_s = cwd.to_string_lossy().into_owned();
    let rules = match pm_dir {
        Some(dir) => live_rules(dir, crate::master::ALIAS, state_dirs)?,
        None => Vec::new(),
    };
    if rules
        .iter()
        .any(|r| r.effect == Effect::Deny && rule_matches(r, argv, &cwd_s))
    {
        return Err(Error::rejected(
            "a deny rule covers this command — it wins over an allow rule",
        ));
    }
    let lock = dir_lock(state_dir);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = load_doc(state_dir)?;
    expire(&mut doc, now);
    // Only redeem grants tied to a real request filed by `master`: forged orphan
    // grants and worker-owned request grants stay invisible to `master_permission_use`.
    let is_master_grant = |request_id: &str| {
        doc.requests
            .iter()
            .any(|r| r.id == request_id && r.requested_by == crate::master::ALIAS)
    };
    let matched_idx = doc.grants.iter().position(|g| {
        g.uses_left > 0
            && g.expires_at > now
            && g.argv == argv
            && g.cwd == cwd_s
            && is_master_grant(&g.request_id)
    });
    if let Some(idx) = matched_idx {
        let request_id = doc.grants[idx].request_id.clone();
        if consume {
            doc.grants[idx].uses_left = 0;
            save_doc(state_dir, &doc)?;
        }
        return Ok(Some(Use::Grant { request_id }));
    }
    if consume {
        save_doc(state_dir, &doc)?;
    }
    if let Some(rule) = rules
        .iter()
        .find(|r| r.effect == Effect::Allow && rule_matches(r, argv, &cwd_s))
    {
        return Ok(Some(Use::Rule {
            id: rule.id.clone(),
        }));
    }
    Ok(None)
}

#[derive(Clone, Debug)]
pub enum WorkerEval {
    Allowed {
        rule_id: String,
        argv: Vec<String>,
        cwd: String,
    },
    Pending(Request),
}

fn clip_reason(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.len() <= REASON_MAX {
        return trimmed.to_string();
    }
    let mut end = REASON_MAX;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    trimmed[..end].to_string()
}

fn is_prose_body_field(tool: &str, key: &str) -> bool {
    (matches!(tool, "Write" | "TodoWrite") && key == "content")
        || (matches!(tool, "Edit" | "MultiEdit") && matches!(key, "old_string" | "new_string"))
        || (tool == "NotebookEdit" && key == "new_source")
}

fn collect_input_paths(tool: &str, val: &Value, depth: usize, out: &mut Vec<String>) -> Result<()> {
    if depth > 5 {
        return Err(Error::rejected(
            "tool input nesting exceeds maximum depth (5)",
        ));
    }
    match val {
        Value::Object(map) => {
            for (k, v) in map {
                if is_prose_body_field(tool, k) && v.is_string() {
                    continue;
                }
                collect_input_paths(tool, v, depth + 1, out)?;
            }
        }
        Value::Array(arr) => {
            for item in arr {
                collect_input_paths(tool, item, depth + 1, out)?;
            }
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        _ => {}
    }
    Ok(())
}

const WORKER_NON_BASH_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "Glob",
    "Grep",
    "LS",
    "WebFetch",
    "WebSearch",
    "Task",
    "TodoWrite",
    "Skill",
    "ExitPlanMode",
    "EnterWorktree",
    "AskUserQuestion",
];

fn worker_allowed_non_bash_tool(tool: &str) -> bool {
    if WORKER_NON_BASH_TOOLS.contains(&tool) {
        return true;
    }
    if let Some(rest) = tool.strip_prefix("mcp__") {
        return rest.contains("__")
            && !rest.starts_with('_')
            && !rest.ends_with('_')
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    }
    false
}

fn has_command_field(val: &Value, depth: usize) -> bool {
    if depth > 5 {
        return true;
    }
    match val {
        Value::Object(map) => map
            .iter()
            .any(|(k, v)| k == "command" || has_command_field(v, depth + 1)),
        Value::Array(arr) => arr.iter().any(|v| has_command_field(v, depth + 1)),
        _ => false,
    }
}

fn worker_argv_and_reason(
    tool: &str,
    input: &Value,
    input_summary: &str,
) -> Result<(Vec<String>, String)> {
    let tool = tool.trim();
    if tool.is_empty()
        || tool.len() > 128
        || tool
            .chars()
            .any(|c| c.is_whitespace() || c == '*' || c.is_control())
    {
        return Err(Error::rejected("invalid tool name in permission request"));
    }
    if tool == "Bash" {
        let cmd = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if cmd.is_empty() || cmd.len() > CMD_MAX {
            return Err(Error::rejected(
                "one plain Bash command (1-4096 bytes) is required",
            ));
        }
        if cmd.chars().any(|c| {
            matches!(
                c,
                '|' | ';'
                    | '&'
                    | '<'
                    | '>'
                    | '`'
                    | '$'
                    | '('
                    | ')'
                    | '\''
                    | '"'
                    | '\\'
                    | '\n'
                    | '\r'
                    | '*'
                    | '?'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '#'
                    | '!'
            )
        }) {
            return Err(Error::rejected(
                "no pipes, chaining (`;`, `&&`, `||`), redirects, `$(…)`, backticks, quotes, globs or escapes — run one plain command",
            ));
        }
        let tokens: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
        if tokens.is_empty() || tokens.len() > ARG_MAX || !tokens.iter().all(|a| pattern_arg_ok(a))
        {
            return Err(Error::rejected(
                "one plain command, 1-64 arguments with rule-safe characters — no quotes or chaining",
            ));
        }
        let argv = normalize_argv(&tokens);
        let reason = input
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(clip_reason)
            .unwrap_or_else(|| "Run shell command".to_string());
        return Ok((argv, reason));
    }

    if !worker_allowed_non_bash_tool(tool) {
        return Err(Error::rejected(format!(
            "tool '{tool}' is not an allowed non-Bash worker tool — run shell and CLI commands through Bash"
        )));
    }
    if has_command_field(input, 0) {
        return Err(Error::rejected(
            "non-Bash tool request cannot carry a command field",
        ));
    }

    let target = ["file_path", "notebook_path", "path", "url", "pattern"]
        .into_iter()
        .find_map(|key| {
            input
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            let trimmed = input_summary.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .unwrap_or("{}");
    if target.len() > ARG_LEN || target.chars().any(char::is_control) {
        return Err(Error::rejected(format!(
            "{tool} target exceeds {ARG_LEN} bytes or contains control characters"
        )));
    }
    let summary = input_summary.trim();
    let reason = if summary.is_empty() {
        format!("Use {tool}")
    } else {
        clip_reason(&format!("{tool}: {summary}"))
    };
    Ok((vec![tool.to_string(), target.to_string()], reason))
}

fn worker_risk(argv: &[String]) -> Risk {
    match argv.first().map(String::as_str) {
        Some("Read" | "Glob" | "Grep") => Risk::Low,
        Some("Write" | "Edit" | "MultiEdit" | "NotebookEdit") => Risk::High,
        _ => risk_of(argv),
    }
}

/// Evaluate a brokered worker permission request (`kind == "approval"`).
/// Enforces `Class::Never` and fail-closed `agents/<alias>/permissions.yaml`
/// rules (Deny > Allow) BEFORE parking a pending request or returning an immediate allow.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_worker(
    state_dir: &Path,
    pm_dir: Option<&Path>,
    alias: &str,
    handle: &str,
    tool: &str,
    input: &Value,
    input_summary: &str,
    cwd: &Path,
    state_dirs: &[&Path],
    now: i64,
) -> Result<WorkerEval> {
    alias_ok(alias)?;
    if alias == crate::master::ALIAS {
        return Err(Error::rejected(
            "worker permission evaluation cannot run for master",
        ));
    }
    let cwd = canonical_cwd(cwd)?;
    let cwd_s = cwd.to_string_lossy().into_owned();
    let (argv, reason) = worker_argv_and_reason(tool, input, input_summary)?;
    if let Some(why) = never_why_for(&argv, &cwd, state_dirs, false) {
        return Err(Error::rejected(format!(
            "{why} — it cannot be approved and no permission rule can cover it"
        )));
    }
    if let Some(pm) = pm_dir {
        if argv_touches_pm_agents_dir(&argv, &cwd, pm) {
            return Err(Error::rejected(
                "agent SOUL.md, AGENT.md and permissions.yaml files are never requestable — it cannot be approved and no permission rule can cover it",
            ));
        }
    }
    let mut input_paths = Vec::new();
    collect_input_paths(tool.trim(), input, 0, &mut input_paths)?;
    for path in input_paths {
        if touches_protected_file(&path, &cwd)
            || pm_dir.is_some_and(|pm| touches_pm_agents_dir(&path, &cwd, pm))
        {
            return Err(Error::rejected(
                "agent SOUL.md, AGENT.md and permissions.yaml files are never requestable — it cannot be approved and no permission rule can cover it",
            ));
        }
        if touches_state_path(&path, &cwd, state_dirs, false) {
            return Err(Error::rejected(
                "the daemon state dir is never requestable — it cannot be approved and no permission rule can cover it",
            ));
        }
    }

    let rules = match pm_dir {
        Some(dir) => live_rules(dir, alias, state_dirs)?,
        None => Vec::new(),
    };
    if rules
        .iter()
        .any(|r| r.effect == Effect::Deny && rule_matches(r, &argv, &cwd_s))
    {
        return Err(Error::rejected(
            "a deny rule covers this tool request — it wins over an allow rule",
        ));
    }
    if let Some(rule) = rules
        .iter()
        .find(|r| r.effect == Effect::Allow && rule_matches(r, &argv, &cwd_s))
    {
        return Ok(WorkerEval::Allowed {
            rule_id: rule.id.clone(),
            argv,
            cwd: cwd_s,
        });
    }

    let risk = worker_risk(&argv);
    let req = with_doc(state_dir, now, |doc| {
        if let Some(existing) = doc
            .requests
            .iter()
            .find(|r| r.id == handle && r.status == "pending" && r.requested_by == alias)
        {
            return Ok(existing.clone());
        }
        let req = Request {
            id: handle.to_string(),
            argv,
            cwd: cwd_s,
            reason,
            requested_by: alias.to_string(),
            created: now,
            expires_at: now + TTL_SECS,
            status: "pending".into(),
            risk,
            decision: String::new(),
            decision_label: String::new(),
        };
        doc.requests.push(req.clone());
        Ok(req)
    })?;
    Ok(WorkerEval::Pending(req))
}

/// Synchronize a brokered worker request when settled via `cadence agent respond`.
pub fn settle_from_respond(
    state_dir: &Path,
    id: &str,
    accepted: bool,
    reason: Option<&str>,
    now: i64,
) -> Result<Option<Request>> {
    with_doc(state_dir, now, |doc| {
        let Some(req) = doc
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.status == "pending")
        else {
            return Ok(None);
        };
        if accepted {
            req.status = "allowed".into();
            req.decision = "allow_once".into();
            req.decision_label = format!("Allowed once {}", decision_hm(now));
        } else {
            req.status = "rejected".into();
            req.decision = "reject".into();
            req.decision_label = match reason.map(str::trim).filter(|s| !s.is_empty()) {
                Some(why) => format!("Rejected: {why}"),
                None => "Rejected".into(),
            };
        }
        Ok(Some(req.clone()))
    })
}

/// Mark a still-pending request expired when `request_close` times out on the client side.
pub fn expire_by_id(state_dir: &Path, id: &str, now: i64) -> Result<()> {
    with_doc(state_dir, now, |doc| {
        if let Some(req) = doc
            .requests
            .iter_mut()
            .find(|r| r.id == id && r.status == "pending")
        {
            req.status = "expired".into();
        }
        Ok(())
    })
}

/// Requests the board still shows: pending ones, and decided ones that
/// have not expired, so the chat card and the rail update in place.
pub fn board_requests(state_dir: &Path, now: i64) -> Result<Vec<Request>> {
    let lock = dir_lock(state_dir);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = load_doc(state_dir)?;
    expire(&mut doc, now);
    save_doc(state_dir, &doc)?;
    Ok(doc
        .requests
        .into_iter()
        .filter(|r| matches!(r.status.as_str(), "pending" | "allowed" | "rejected"))
        .collect())
}

/// A narrowed prefix the board can offer: the last path argument with
/// its final segment replaced by `*`. `None` when that would not be
/// narrower than the verb.
pub fn suggest_tail_for(argv: &[String], requested_by: &str) -> Option<Vec<String>> {
    let min_len = if requested_by == crate::master::ALIAS
        || is_cadence(argv.first().map(String::as_str).unwrap_or(""))
    {
        3
    } else {
        2
    };
    if argv.len() < min_len {
        return None;
    }
    let last = argv.last()?;
    if last.starts_with('-') || last.contains('*') || !last.contains('/') {
        return None;
    }
    let path = Path::new(last);
    let parent = path.parent()?.to_str()?;
    if parent.is_empty() || parent == "/" {
        return None;
    }
    let pat = format!("{parent}/*");
    validate_pattern(&pat).ok()?;
    Some(vec![pat])
}

pub fn suggest_tail(argv: &[String]) -> Option<Vec<String>> {
    suggest_tail_for(argv, crate::master::ALIAS)
}

pub fn request_json(req: &Request) -> Value {
    json!({
        "id": req.id,
        "argv": req.argv,
        "cwd": req.cwd,
        "reason": req.reason,
        "requested_by": req.requested_by,
        "created": req.created,
        "expires_at": req.expires_at,
        "status": req.status,
        "decision": req.decision,
        "decision_label": req.decision_label,
        "risk": req.risk.as_str(),
        "command": req.argv.join(" "),
        "prefix": suggest_tail_for(&req.argv, &req.requested_by),
    })
}

pub fn rule_json_for(rule: &Rule, agent: &str) -> Value {
    json!({
        "id": rule.id,
        "agent": agent,
        "effect": match rule.effect { Effect::Allow => "allow", Effect::Deny => "deny" },
        "scope": match rule.scope { Scope::Exact => "exact", Scope::Prefix => "prefix" },
        "argv": rule.argv,
        "tail": rule.tail,
        "cwd": rule.cwd,
        "by": rule.by,
        "at": rule.at,
    })
}

pub fn rule_json(rule: &Rule) -> Value {
    rule_json_for(rule, crate::master::ALIAS)
}

/// `HH:MM` from an epoch second. The card shows this next to the
/// operator, in UTC, because the daemon has no operator timezone.
fn decision_hm(now: i64) -> String {
    let day = now.rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

/// The wall clock the daemon uses. Tests pass their own `now`.
pub fn clock() -> i64 {
    now_epoch()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    fn tmp() -> tempfile::TempDir {
        tempfile::TempDir::new().unwrap()
    }

    fn checkout(dir: &Path) -> PathBuf {
        let c = dir.join("repo");
        fs::create_dir_all(c.join("src")).unwrap();
        fs::canonicalize(&c).unwrap()
    }

    #[test]
    fn pipes_and_never_list_are_not_requestable() {
        let dir = tmp();
        let cwd = dir.path();
        let state = cwd;
        let bad = classify(
            &["cadence".into(), "issue".into(), "ls;rm".into()],
            cwd,
            &[],
            &[state],
        );
        assert!(matches!(bad, Class::IllFormed { .. }), "{bad:?}");
        for argv in [
            vec!["cadence".into(), "delivery".into(), "merge".into()],
            vec!["cadence".into(), "rollout".into(), "status".into()],
            vec!["cadence".into(), "daemon".into(), "restart".into()],
            vec!["cadence".into(), "secret".into(), "ls".into()],
            vec!["cadence".into(), "audit".into(), "approve".into()],
            vec!["cadence".into(), "audit".into(), "designate".into()],
            vec![
                "cadence".into(),
                "master".into(),
                "edit".into(),
                "agents/master/SOUL.md".into(),
            ],
            vec![
                "cadence".into(),
                "wiki".into(),
                "put".into(),
                "agents/master/permissions.yaml".into(),
            ],
        ] {
            let class = classify(&argv, cwd, &[], &[state]);
            assert!(
                matches!(class, Class::Never { .. }),
                "{argv:?} -> {class:?}"
            );
        }
    }

    #[test]
    fn state_dir_and_foreign_programs_are_never() {
        let dir = tmp();
        let state = dir.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let cwd = dir.path();
        let class = classify(
            &["cat".into(), state.to_str().unwrap().into()],
            cwd,
            &[checkout(dir.path())],
            &[state.as_path()],
        );
        assert!(matches!(class, Class::Never { .. }), "{class:?}");
        let class = classify(
            &["rm".into(), "-rf".into(), "/".into()],
            cwd,
            &[],
            &[state.as_path()],
        );
        assert!(matches!(class, Class::Never { .. }), "{class:?}");
    }

    #[test]
    fn readonly_inside_a_checkout_is_requestable() {
        let dir = tmp();
        let state = dir.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let repo = checkout(dir.path());
        let file = repo.join("src").join("a.rs");
        fs::write(&file, "x").unwrap();
        let class = classify(
            &["cat".into(), file.to_str().unwrap().into()],
            dir.path(),
            &[repo],
            &[state.as_path()],
        );
        assert!(
            matches!(class, Class::Requestable { risk: Risk::Low }),
            "{class:?}"
        );
        let outside = dir.path().join("nope");
        fs::write(&outside, "x").unwrap();
        let class = classify(
            &["cat".into(), outside.to_str().unwrap().into()],
            dir.path(),
            &[checkout(dir.path())],
            &[state.as_path()],
        );
        assert!(matches!(class, Class::Never { .. }), "{class:?}");
    }

    #[test]
    fn agent_cannot_ask_and_forged_id_is_refused() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let err = ask(
            dir.path(),
            &["cadence".into(), "issue".into(), "new".into(), "T".into()],
            &cwd,
            "need a ticket",
            "worker",
            &[],
            &[dir.path()],
            1_000,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("only the master"), "{err}");
        let err = allow_once(dir.path(), "mp-forged", &[], &[dir.path()], 1_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no permission request"), "{err}");
    }

    #[test]
    fn single_use_expiry_and_argv_binding() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "issue".into(),
            "comment".into(),
            "Title".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "file the ticket",
            "master",
            &[],
            &[dir.path()],
            1_000,
        )
        .unwrap();
        allow_once(dir.path(), &req.id, &[], &[dir.path()], 1_000).unwrap();
        let other = vec![
            "cadence".into(),
            "issue".into(),
            "comment".into(),
            "Other".into(),
        ];
        let err = take(dir.path(), None, &other, &cwd, &[], &[dir.path()], 1_001)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no live permission"), "{err}");
        let elsewhere = dir.path().join("other");
        fs::create_dir_all(&elsewhere).unwrap();
        let err = take(
            dir.path(),
            None,
            &argv,
            &elsewhere,
            &[],
            &[dir.path()],
            1_002,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no live permission"), "{err}");
        let used = take(dir.path(), None, &argv, &cwd, &[], &[dir.path()], 1_003).unwrap();
        assert!(matches!(used, Use::Grant { .. }));
        let err = take(dir.path(), None, &argv, &cwd, &[], &[dir.path()], 1_004)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no live permission"), "{err}");
        // A fresh grant that is already expired does not match.
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "again",
            "master",
            &[],
            &[dir.path()],
            2_000,
        )
        .unwrap();
        allow_once(dir.path(), &req.id, &[], &[dir.path()], 2_000).unwrap();
        let err = take(
            dir.path(),
            None,
            &argv,
            &cwd,
            &[],
            &[dir.path()],
            2_000 + TTL_SECS,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no live permission"), "{err}");
    }

    #[test]
    fn concurrent_single_use_succeeds_once() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "issue".into(),
            "comment".into(),
            "Once".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "status",
            "master",
            &[],
            &[dir.path()],
            5_000,
        )
        .unwrap();
        allow_once(dir.path(), &req.id, &[], &[dir.path()], 5_000).unwrap();
        let wins = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..8 {
            let wins = Arc::clone(&wins);
            let state = dir.path().to_path_buf();
            let cwd = cwd.clone();
            let argv = argv.clone();
            threads.push(thread::spawn(move || {
                if take(&state, None, &argv, &cwd, &[], &[state.as_path()], 5_001).is_ok() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            }));
        }
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(wins.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn planted_grant_cannot_cover_the_never_list() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec!["cadence".into(), "audit".into(), "approve".into()];
        let mut doc = Doc::default();
        doc.grants.push(Grant {
            id: "mg-planted".into(),
            request_id: "mp-planted".into(),
            argv: argv.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
            uses_left: 1,
            expires_at: 9_000 + TTL_SECS,
        });
        save_doc(dir.path(), &doc).unwrap();
        let err = take(dir.path(), None, &argv, &cwd, &[], &[dir.path()], 9_000)
            .unwrap_err()
            .to_string();
        assert!(err.contains("never requestable"), "{err}");
        assert!(err.contains("cannot allow"), "{err}");
    }

    #[test]
    fn wildcard_shape_is_rejected_and_deny_beats_allow() {
        let err = validate_pattern("foo*bar").unwrap_err().to_string();
        assert!(err.contains("end of the argument"), "{err}");
        let err = validate_pattern("*foo").unwrap_err().to_string();
        assert!(err.contains("end of the argument"), "{err}");
        let err = check_rule_shape(
            &["cadence".into(), "*".into()],
            &["x".into()],
            &Scope::Prefix,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("different command"), "{err}");

        let dir = tmp();
        let pm = dir.path().join("pm");
        fs::create_dir_all(&pm).unwrap();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "wiki".into(),
            "edit".into(),
            "agents/master/knowledge/a.md".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "write a note",
            "master",
            &[],
            &[dir.path()],
            3_000,
        )
        .unwrap();
        let (_req, allow) = always_rule(
            dir.path(),
            &req.id,
            Scope::Prefix,
            &["agents/master/knowledge/*".into()],
            &[],
            &[dir.path()],
            3_000,
        )
        .unwrap();
        let deny = Rule {
            id: "mr-deny".into(),
            effect: Effect::Deny,
            scope: Scope::Exact,
            argv: argv.clone(),
            tail: vec![],
            cwd: cwd.to_string_lossy().into_owned(),
            by: "operator".into(),
            at: 3_000,
        };
        write_rules(&pm, "master", &[allow, deny]).unwrap();
        let err = take(
            dir.path(),
            Some(pm.as_path()),
            &argv,
            &cwd,
            &[],
            &[dir.path()],
            3_001,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("deny rule"), "{err}");
        revoke_rule(&pm, "master", "mr-deny").unwrap();
        let used = take(
            dir.path(),
            Some(pm.as_path()),
            &argv,
            &cwd,
            &[],
            &[dir.path()],
            3_002,
        )
        .unwrap();
        assert!(matches!(used, Use::Rule { .. }), "{used:?}");
        // A sibling path matches the prefix; a different verb does not.
        let sibling = vec![
            "cadence".into(),
            "wiki".into(),
            "edit".into(),
            "agents/master/knowledge/b.md".into(),
        ];
        assert!(peek(
            dir.path(),
            Some(pm.as_path()),
            &sibling,
            &cwd,
            &[],
            &[dir.path()],
            3_003,
        )
        .unwrap());
        let other_verb = vec![
            "cadence".into(),
            "issue".into(),
            "new".into(),
            "agents/master/knowledge/b.md".into(),
        ];
        assert!(!peek(
            dir.path(),
            Some(pm.as_path()),
            &other_verb,
            &cwd,
            &[],
            &[dir.path()],
            3_004,
        )
        .unwrap());
    }

    #[test]
    fn revoked_rule_stops_matching() {
        let dir = tmp();
        let pm = dir.path().join("pm");
        fs::create_dir_all(&pm).unwrap();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "issue".into(),
            "comment".into(),
            "Read".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "read it",
            "master",
            &[],
            &[dir.path()],
            4_000,
        )
        .unwrap();
        let (_r, rule) = always_rule(
            dir.path(),
            &req.id,
            Scope::Exact,
            &[],
            &[],
            &[dir.path()],
            4_000,
        )
        .unwrap();
        write_rules(&pm, "master", std::slice::from_ref(&rule)).unwrap();
        assert!(peek(
            dir.path(),
            Some(pm.as_path()),
            &argv,
            &cwd,
            &[],
            &[dir.path()],
            4_001,
        )
        .unwrap());
        revoke_rule(&pm, "master", &rule.id).unwrap();
        assert!(!peek(
            dir.path(),
            Some(pm.as_path()),
            &argv,
            &cwd,
            &[],
            &[dir.path()],
            4_002,
        )
        .unwrap());
    }

    #[test]
    fn request_expires() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "issue".into(),
            "comment".into(),
            "Later".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "read",
            "master",
            &[],
            &[dir.path()],
            100,
        )
        .unwrap();
        let err = allow_once(dir.path(), &req.id, &[], &[dir.path()], 100 + TTL_SECS)
            .unwrap_err()
            .to_string();
        assert!(err.contains("expired"), "{err}");
        let _ = Duration::from_millis(0);
    }

    #[test]
    fn rules_file_is_per_agent_and_a_second_decision_is_refused() {
        let dir = tmp();
        let master = rules_file(dir.path(), "master").unwrap();
        let worker = rules_file(dir.path(), "worker-1").unwrap();
        assert!(master.ends_with("agents/master/permissions.yaml"));
        assert!(worker.ends_with("agents/worker-1/permissions.yaml"));
        assert!(rules_file(dir.path(), "../master").is_err());
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let req = ask(
            dir.path(),
            &[
                "cadence".into(),
                "issue".into(),
                "comment".into(),
                "T".into(),
            ],
            &cwd,
            "need a ticket",
            "master",
            &[],
            &[],
            1_000,
        )
        .unwrap();
        allow_once(dir.path(), &req.id, &[], &[dir.path()], 1_000).unwrap();
        let again = allow_once(dir.path(), &req.id, &[], &[dir.path()], 1_001)
            .unwrap_err()
            .to_string();
        assert!(again.contains("cannot be decided again"), "{again}");
        let listed = board_requests(dir.path(), 1_002).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(
            listed[0]
                .decision_label
                .starts_with("Allowed once by operator "),
            "{}",
            listed[0].decision_label
        );
    }

    /// The master's tool line names this daemon's state dir, and plan
    /// files live under `state/master/tmp`. Those are not a reach into
    /// the state dir. A different path that resolves into state still is.
    #[test]
    fn own_state_dir_and_master_scratch_are_not_untouchable() {
        let dir = tmp();
        let state = dir.path().join("state");
        let scratch = state.join("master").join("tmp");
        fs::create_dir_all(&scratch).unwrap();
        let plan = scratch.join("plan.md");
        fs::write(&plan, "# plan\n").unwrap();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let state = fs::canonicalize(&state).unwrap();
        let plan = fs::canonicalize(&plan).unwrap();
        let argv = vec![
            "cadence".into(),
            "--state-dir".into(),
            state.to_string_lossy().into_owned(),
            "plan".into(),
            "propose".into(),
            "--file".into(),
            plan.to_string_lossy().into_owned(),
        ];
        let class = classify(&argv, &cwd, &[], &[state.as_path()]);
        assert!(
            matches!(class, Class::Allowlisted),
            "own --state-dir and master/tmp must stay allowlisted, got {class:?}"
        );
        let secret = state.join("secrets");
        fs::create_dir_all(&secret).unwrap();
        let secret_file = secret.join("token");
        fs::write(&secret_file, "x").unwrap();
        let argv = vec![
            "cadence".into(),
            "--state-dir".into(),
            state.to_string_lossy().into_owned(),
            "wiki".into(),
            "put".into(),
            secret_file.to_string_lossy().into_owned(),
        ];
        let class = classify(&argv, &cwd, &[], &[state.as_path()]);
        assert!(
            matches!(class, Class::Never { .. }),
            "a path elsewhere in state stays never-requestable, got {class:?}"
        );
    }

    /// A prefix rule matches on a path-segment boundary and never across
    /// `.` or `..`. A cleaned `agents/master/../SOUL.md` is protected.
    #[test]
    fn prefix_rule_rejects_dot_segments_and_mid_segment() {
        let dir = tmp();
        let pm = dir.path().join("pm");
        fs::create_dir_all(&pm).unwrap();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "wiki".into(),
            "edit".into(),
            "agents/master/knowledge/a.md".into(),
        ];
        let req = ask(
            dir.path(),
            &argv,
            &cwd,
            "write a page",
            "master",
            &[],
            &[dir.path()],
            3_000,
        )
        .unwrap();
        let (_req, allow) = always_rule(
            dir.path(),
            &req.id,
            Scope::Prefix,
            &["agents/master/knowledge/*".into()],
            &[],
            &[dir.path()],
            3_000,
        )
        .unwrap();
        write_rules(&pm, "master", &[allow]).unwrap();
        let crossed = vec![
            "cadence".into(),
            "wiki".into(),
            "edit".into(),
            "agents/master/knowledge/../knowledge/b.md".into(),
        ];
        assert!(
            !peek(
                dir.path(),
                Some(pm.as_path()),
                &crossed,
                &cwd,
                &[],
                &[dir.path()],
                3_001,
            )
            .unwrap(),
            "a .. segment must not match a prefix rule"
        );
        let wide = Rule {
            id: "mr-wide".into(),
            effect: Effect::Allow,
            scope: Scope::Prefix,
            argv: vec!["cadence".into(), "wiki".into(), "edit".into()],
            tail: vec!["agents/master/knowledge*".into()],
            cwd: cwd.to_string_lossy().into_owned(),
            by: "operator".into(),
            at: 3_000,
        };
        write_rules(&pm, "master", &[wide]).unwrap();
        let mid = vec![
            "cadence".into(),
            "wiki".into(),
            "edit".into(),
            "agents/master/knowledgeX".into(),
        ];
        assert!(
            !peek(
                dir.path(),
                Some(pm.as_path()),
                &mid,
                &cwd,
                &[],
                &[dir.path()],
                3_002,
            )
            .unwrap(),
            "knowledge* must not match knowledgeX"
        );
        let soul = classify(
            &[
                "cadence".into(),
                "wiki".into(),
                "put".into(),
                "agents/master/./SOUL.md".into(),
            ],
            &cwd,
            &[],
            &[dir.path()],
        );
        assert!(
            matches!(soul, Class::Never { .. }),
            "a cleaned SOUL.md path is protected, got {soul:?}"
        );
    }

    /// `cadence master edit SOUL.md` is never requestable, and a planted
    /// pending request cannot be allowed once.
    #[test]
    fn master_edit_of_soul_is_never_and_allow_once_rechecks() {
        let dir = tmp();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let argv = vec![
            "cadence".into(),
            "master".into(),
            "edit".into(),
            "SOUL.md".into(),
            "--file".into(),
            "replacement.md".into(),
        ];
        let class = classify(&argv, &cwd, &[], &[dir.path()]);
        assert!(matches!(class, Class::Never { .. }), "{class:?}");
        let mut doc = Doc::default();
        doc.requests.push(Request {
            id: "mp-soul".into(),
            argv: argv.clone(),
            cwd: cwd.to_string_lossy().into_owned(),
            reason: "planted".into(),
            requested_by: "master".into(),
            created: 1_000,
            expires_at: 1_000 + TTL_SECS,
            status: "pending".into(),
            risk: Risk::High,
            decision: String::new(),
            decision_label: String::new(),
        });
        save_doc(dir.path(), &doc).unwrap();
        let err = allow_once(dir.path(), "mp-soul", &[], &[dir.path()], 1_001)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("never requestable") || err.contains("cannot be approved"),
            "{err}"
        );
    }

    /// A hand-edited `*` tail and a never-list allow rule are not live.
    #[test]
    fn load_rules_drops_bad_shape_and_the_never_list() {
        let dir = tmp();
        let pm = dir.path().join("pm");
        fs::create_dir_all(pm.join("agents/master")).unwrap();
        let cwd = fs::canonicalize(dir.path()).unwrap();
        let cwd_s = cwd.to_string_lossy().into_owned();
        let star = Rule {
            id: "mr-star".into(),
            effect: Effect::Allow,
            scope: Scope::Prefix,
            argv: vec!["cadence".into(), "wiki".into(), "put".into()],
            tail: vec!["*".into()],
            cwd: cwd_s.clone(),
            by: "operator".into(),
            at: 1,
        };
        let soul = Rule {
            id: "mr-soul".into(),
            effect: Effect::Allow,
            scope: Scope::Exact,
            argv: vec![
                "cadence".into(),
                "master".into(),
                "edit".into(),
                "AGENT.md".into(),
            ],
            tail: vec![],
            cwd: cwd_s.clone(),
            by: "operator".into(),
            at: 1,
        };
        let mid = Rule {
            id: "mr-mid".into(),
            effect: Effect::Allow,
            scope: Scope::Prefix,
            argv: vec!["cadence".into(), "wiki".into(), "put".into()],
            tail: vec!["agents/master/know*ledge".into()],
            cwd: cwd_s,
            by: "operator".into(),
            at: 1,
        };
        // write_rules goes through serde, which will store the bad tails.
        // Bypass check_rule_shape the way a hand edit does.
        let path = rules_file(&pm, "master").unwrap();
        fs::write(
            &path,
            serde_yaml::to_string(&RulesFile {
                rules: vec![star, soul, mid],
            })
            .unwrap(),
        )
        .unwrap();
        let live = live_rules(&pm, "master", &[dir.path()]).unwrap();
        assert!(
            live.is_empty(),
            "empty stem, mid-argument * and a never-list rule must not load: {live:?}"
        );
        let argv = vec![
            "cadence".into(),
            "wiki".into(),
            "put".into(),
            "agents/master/knowledge/a.md".into(),
        ];
        assert!(
            !peek(
                dir.path(),
                Some(pm.as_path()),
                &argv,
                &cwd,
                &[],
                &[dir.path()],
                2,
            )
            .unwrap(),
            "a bare * must not allow the command"
        );
    }

    #[test]
    fn worker_evaluation_enforces_rules_deny_wins_and_never_list() {
        let dir = tmp();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&pm).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let cwd = fs::canonicalize(&repo).unwrap();
        let cwd_s = cwd.to_string_lossy().into_owned();

        // Write an exact Allow rule, a prefix Allow rule, plus a Deny rule that overlaps
        // the prefix rule on `git status src/secret.rs` (verifying Deny wins over Allow).
        let allow_exact = Rule {
            id: "wr-allow-exact".into(),
            effect: Effect::Allow,
            scope: Scope::Exact,
            argv: vec!["git".into(), "status".into()],
            tail: vec![],
            cwd: cwd_s.clone(),
            by: "operator".into(),
            at: 100,
        };
        let allow_prefix = Rule {
            id: "wr-allow-prefix".into(),
            effect: Effect::Allow,
            scope: Scope::Prefix,
            argv: vec!["git".into(), "status".into()],
            tail: vec!["src/*".into()],
            cwd: cwd_s.clone(),
            by: "operator".into(),
            at: 101,
        };
        let deny_rule = Rule {
            id: "wr-deny".into(),
            effect: Effect::Deny,
            scope: Scope::Exact,
            argv: vec!["git".into(), "status".into(), "src/secret.rs".into()],
            tail: vec![],
            cwd: cwd_s,
            by: "operator".into(),
            at: 102,
        };
        write_rules(&pm, "w1", &[allow_exact, allow_prefix, deny_rule]).unwrap();

        let eval_allow = evaluate_worker(
            &state,
            Some(pm.as_path()),
            "w1",
            "h-allow",
            "Bash",
            &serde_json::json!({"command": "git status"}),
            "git status",
            &cwd,
            &[state.as_path()],
            1_000,
        )
        .unwrap();
        assert!(matches!(
            eval_allow,
            WorkerEval::Allowed { ref rule_id, .. } if rule_id == "wr-allow-exact"
        ));

        let eval_prefix = evaluate_worker(
            &state,
            Some(pm.as_path()),
            "w1",
            "h-prefix",
            "Bash",
            &serde_json::json!({"command": "git status src/lib.rs"}),
            "git status src/lib.rs",
            &cwd,
            &[state.as_path()],
            1_001,
        )
        .unwrap();
        assert!(matches!(
            eval_prefix,
            WorkerEval::Allowed { ref rule_id, .. } if rule_id == "wr-allow-prefix"
        ));

        let err_deny = evaluate_worker(
            &state,
            Some(pm.as_path()),
            "w1",
            "h-deny",
            "Bash",
            &serde_json::json!({"command": "git status src/secret.rs"}),
            "git status src/secret.rs",
            &cwd,
            &[state.as_path()],
            1_002,
        )
        .unwrap_err()
        .to_string();
        assert!(err_deny.contains("deny"), "{err_deny}");

        for cmd in [
            "cadence update",
            "gh pr merge 123",
            "git push --force origin feat",
        ] {
            let err = evaluate_worker(
                &state,
                Some(pm.as_path()),
                "w1",
                "h-never",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &cwd,
                &[state.as_path()],
                1_002,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("cannot be approved"), "{cmd}: {err}");
        }

        for protected in [
            "agents/w2/SOUL.md",
            "agents/w1/permissions.yaml",
            "agents/w1/AGENT.md",
        ] {
            let err = evaluate_worker(
                &state,
                Some(pm.as_path()),
                "w1",
                "h-prot",
                "Write",
                &serde_json::json!({"file_path": protected}),
                protected,
                &cwd,
                &[state.as_path()],
                1_003,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("never requestable"), "{protected}: {err}");
        }
    }

    #[test]
    fn worker_allow_once_and_rules_never_mint_or_satisfy_master_grants() {
        let dir = tmp();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&pm).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let cwd = fs::canonicalize(&repo).unwrap();

        let eval = evaluate_worker(
            &state,
            Some(pm.as_path()),
            "w1",
            "req-w1",
            "Bash",
            &serde_json::json!({"command": "cadence issue show CAD-1"}),
            "cadence issue show CAD-1",
            &cwd,
            &[state.as_path()],
            2_000,
        )
        .unwrap();
        let req = match eval {
            WorkerEval::Pending(r) => r,
            other => panic!("expected Pending, got {other:?}"),
        };
        assert_eq!(req.requested_by, "w1");

        // allow_once on a worker request marks it approved but mints NO Master Grant.
        let approved = allow_once(&state, &req.id, &[], &[state.as_path()], 2_001).unwrap();
        assert_eq!(approved.status, "allowed");
        let doc = load_doc(&state).unwrap();
        assert!(
            doc.grants.is_empty(),
            "worker request minted a grant: {:?}",
            doc.grants
        );

        // Even an explicit Always rule on w1 must not satisfy Master's decide_use.
        let eval2 = evaluate_worker(
            &state,
            Some(pm.as_path()),
            "w1",
            "req-w1-always",
            "Bash",
            &serde_json::json!({"command": "cadence issue show CAD-1"}),
            "cadence issue show CAD-1",
            &cwd,
            &[state.as_path()],
            2_003,
        )
        .unwrap();
        let req2 = match eval2 {
            WorkerEval::Pending(r) => r,
            other => panic!("expected Pending, got {other:?}"),
        };
        let (_approved2, rule) = always_rule(
            &state,
            &req2.id,
            Scope::Exact,
            &[],
            &[],
            &[state.as_path()],
            2_004,
        )
        .unwrap();
        write_rules(&pm, "w1", &[rule]).unwrap();

        let argv = vec![
            "cadence".into(),
            "issue".into(),
            "show".into(),
            "CAD-1".into(),
        ];
        assert!(
            decide_use(
                &state,
                Some(pm.as_path()),
                &argv,
                &cwd,
                &[],
                &[state.as_path()],
                2_005,
                true,
            )
            .unwrap()
            .is_none(),
            "worker allow rule must not authorize Master"
        );
    }

    #[test]
    fn worker_symlink_and_dangling_symlink_to_protected_files_are_never() {
        let dir = tmp();
        let state = dir.path().join("state");
        fs::create_dir_all(&state).unwrap();
        let pm = dir.path().join("pm");
        let w1_dir = pm.join("agents/w1");
        let w2_dir = pm.join("agents/w2");
        fs::create_dir_all(&w1_dir).unwrap();
        fs::create_dir_all(&w2_dir).unwrap();
        let w2_perm = w2_dir.join("permissions.yaml");
        fs::write(&w2_perm, "rules: []\n").unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap();
        let cwd = fs::canonicalize(&repo).unwrap();

        std::os::unix::fs::symlink(&w1_dir, cwd.join("cfg")).unwrap();
        // Point dangling symlink into a not-yet-created subdirectory/file under pm/agents.
        let uncreated_perm = pm.join("agents/w3/permissions.yaml");
        std::os::unix::fs::symlink(&uncreated_perm, cwd.join("dangling.yaml")).unwrap();
        std::os::unix::fs::symlink(&w2_perm, cwd.join("live.yaml")).unwrap();

        // Plant a broad prefix rule for Write <cwd>/cfg/*.
        let prefix_rule = Rule {
            id: "mr-cfg-prefix".into(),
            effect: Effect::Allow,
            scope: Scope::Prefix,
            argv: vec!["Write".into()],
            tail: vec![format!("{}/cfg/*", cwd.display())],
            cwd: cwd.to_string_lossy().into_owned(),
            by: "operator".into(),
            at: 1_000,
        };
        write_rules(&pm, "w1", &[prefix_rule]).unwrap();

        for target in [
            "cfg/permissions.yaml",
            "cfg/SOUL.md",
            "cfg/AGENT.md",
            "cfg/../w2/permissions.yaml",
            "dangling.yaml",
            "live.yaml",
        ] {
            let err = evaluate_worker(
                &state,
                Some(pm.as_path()),
                "w1",
                "h-sym-write",
                "Write",
                &serde_json::json!({"file_path": target, "content": "x"}),
                target,
                &cwd,
                &[state.as_path()],
                1_001,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("never requestable"), "{target}: {err}");

            let cls = classify(
                &["Write".into(), target.into()],
                &cwd,
                &[],
                &[state.as_path()],
            );
            assert!(matches!(cls, Class::Never { .. }), "{target}: {cls:?}");
        }

        // Bash hitting <cwd>/cfg (directory symlink to pm/agents/w1) or --file=<cwd>/dangling.yaml.
        for cmd in [
            "cp /tmp/x cfg".to_string(),
            format!(
                "cadence --file={} --help",
                cwd.join("dangling.yaml").display()
            ),
        ] {
            let err = evaluate_worker(
                &state,
                Some(pm.as_path()),
                "w1",
                "h-sym-bash",
                "Bash",
                &serde_json::json!({"command": cmd}),
                &cmd,
                &cwd,
                &[state.as_path()],
                1_002,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("never requestable") || err.contains("cannot be approved"),
                "{cmd}: {err}"
            );
        }

        // Even if a pending request was somehow planted, allow_once and always_rule must refuse it.
        let mut doc = Doc::default();
        doc.requests.push(Request {
            id: "mp-sym-dangling".into(),
            argv: vec![
                "cadence".into(),
                format!("--file={}", cwd.join("dangling.yaml").display()),
            ],
            cwd: cwd.to_string_lossy().into_owned(),
            reason: "planted".into(),
            requested_by: "w1".into(),
            created: 1_000,
            expires_at: 1_000 + TTL_SECS,
            status: "pending".into(),
            risk: Risk::High,
            decision: String::new(),
            decision_label: String::new(),
        });
        save_doc(&state, &doc).unwrap();
        allow_once(&state, "mp-sym-dangling", &[], &[state.as_path()], 1_003)
            .expect_err("allow_once must refuse dangling symlink to protected permissions.yaml");
        always_rule(
            &state,
            "mp-sym-dangling",
            Scope::Exact,
            &[],
            &[],
            &[state.as_path()],
            1_003,
        )
        .expect_err("always_rule must refuse dangling symlink to protected permissions.yaml");
    }

    #[test]
    fn cadence_wake_timeout_and_global_flags_never_bypass_update() {
        let dir = tmp();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&pm).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let cwd = fs::canonicalize(&repo).unwrap();

        for cmd in [
            "cadence --wake-timeout 10 update",
            "cadence --wake-timeout=10 --org fav-limited update",
            "cadence -- update",
        ] {
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            assert!(
                matches!(
                    classify(&argv, &cwd, &[], &[state.as_path()]),
                    Class::Never { .. }
                ),
                "expected Class::Never for `{cmd}`"
            );
            let err = evaluate_worker(
                &state,
                Some(pm.as_path()),
                "w1",
                "h-wake-update",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &cwd,
                &[state.as_path()],
                1_005,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("never requestable"), "{cmd}: {err}");
        }

        let allowed_argv: Vec<String> = "cadence --wake-timeout 10 issue show CAD-616"
            .split_whitespace()
            .map(str::to_string)
            .collect();
        assert!(
            !matches!(
                classify(&allowed_argv, &cwd, &[], &[state.as_path()]),
                Class::Never { .. }
            ),
            "issue show with --wake-timeout must not be Class::Never"
        );
    }

    #[test]
    fn worker_state_dir_cwd_rhs_symlink_and_loop_are_never() {
        let dir = tmp();
        let state = dir.path().join("state");
        let master_cwd = state.join("master/cwd");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&master_cwd).unwrap();
        fs::create_dir_all(&pm).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let state_canon = fs::canonicalize(&state).unwrap();
        let master_cwd_canon = fs::canonicalize(&master_cwd).unwrap();
        let repo_cwd = fs::canonicalize(&repo).unwrap();
        fs::write(state_canon.join("cadence.sqlite3"), "sqlite").unwrap();
        std::os::unix::fs::symlink(&state_canon, repo_cwd.join("st")).unwrap();
        std::os::unix::fs::symlink(repo_cwd.join("loop_b"), repo_cwd.join("loop_a")).unwrap();
        std::os::unix::fs::symlink(repo_cwd.join("loop_a"), repo_cwd.join("loop_b")).unwrap();

        let state_db = state_canon.join("cadence.sqlite3");
        let rules = vec![
            Rule {
                id: "wr-state-cwd".into(),
                effect: Effect::Allow,
                scope: Scope::Exact,
                argv: vec!["cat".into(), "cadence.sqlite3".into()],
                tail: vec![],
                cwd: state_canon.to_string_lossy().into_owned(),
                by: "operator".into(),
                at: 100,
            },
            Rule {
                id: "wr-master-cwd".into(),
                effect: Effect::Allow,
                scope: Scope::Exact,
                argv: vec!["cat".into(), "cadence.sqlite3".into()],
                tail: vec![],
                cwd: master_cwd_canon.to_string_lossy().into_owned(),
                by: "operator".into(),
                at: 101,
            },
            Rule {
                id: "wr-rhs-dd".into(),
                effect: Effect::Allow,
                scope: Scope::Exact,
                argv: vec![
                    "dd".into(),
                    format!("if={}", state_db.display()),
                    "of=/tmp/out".into(),
                ],
                tail: vec![],
                cwd: repo_cwd.to_string_lossy().into_owned(),
                by: "operator".into(),
                at: 102,
            },
            Rule {
                id: "wr-sym-st".into(),
                effect: Effect::Allow,
                scope: Scope::Exact,
                argv: vec!["cat".into(), "st/cadence.sqlite3".into()],
                tail: vec![],
                cwd: repo_cwd.to_string_lossy().into_owned(),
                by: "operator".into(),
                at: 103,
            },
        ];
        write_rules(&pm, "w1", &rules).unwrap();
        assert!(
            live_rules(&pm, "w1", &[state_canon.as_path()])
                .unwrap()
                .is_empty(),
            "worker allow rules touching state_dir via cwd, =rhs, or symlink must not be live"
        );

        for (bad_cwd, cmd) in [
            (&state_canon, "cat cadence.sqlite3".to_string()),
            (&master_cwd_canon, "cat cadence.sqlite3".to_string()),
            (
                &repo_cwd,
                format!("dd if={} of=/tmp/out", state_db.display()),
            ),
            (
                &repo_cwd,
                format!("cadence --db={} status", state_db.display()),
            ),
            (&repo_cwd, "cat st/cadence.sqlite3".to_string()),
            (&repo_cwd, "cat loop_a".to_string()),
        ] {
            let err = evaluate_worker(
                &state_canon,
                Some(pm.as_path()),
                "w1",
                "h-state-bash",
                "Bash",
                &serde_json::json!({"command": cmd}),
                &cmd,
                bad_cwd,
                &[state_canon.as_path()],
                1_010,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err.contains("never requestable"),
                "cwd={} cmd={cmd}: {err}",
                bad_cwd.display()
            );
        }

        for target in ["st/cadence.sqlite3", "loop_a"] {
            let err = evaluate_worker(
                &state_canon,
                Some(pm.as_path()),
                "w1",
                "h-state-write",
                "Write",
                &serde_json::json!({"file_path": target, "content": "x"}),
                target,
                &repo_cwd,
                &[state_canon.as_path()],
                1_011,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("never requestable"), "{target}: {err}");
        }

        let mut doc = Doc::default();
        doc.requests.push(Request {
            id: "mp-state-cwd".into(),
            argv: vec!["cat".into(), "cadence.sqlite3".into()],
            cwd: state_canon.to_string_lossy().into_owned(),
            reason: "planted".into(),
            requested_by: "w1".into(),
            created: 1_000,
            expires_at: 1_000 + TTL_SECS,
            status: "pending".into(),
            risk: Risk::High,
            decision: String::new(),
            decision_label: String::new(),
        });
        save_doc(&state_canon, &doc).unwrap();
        allow_once(
            &state_canon,
            "mp-state-cwd",
            &[],
            &[state_canon.as_path()],
            1_012,
        )
        .expect_err("allow_once must refuse worker request whose cwd is state_dir");
        always_rule(
            &state_canon,
            "mp-state-cwd",
            Scope::Exact,
            &[],
            &[],
            &[state_canon.as_path()],
            1_012,
        )
        .expect_err("always_rule must refuse worker request whose cwd is state_dir");
    }

    #[test]
    fn worker_plus_refspec_tool_smuggling_and_nested_agents_dir_are_never() {
        let dir = tmp();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        let w1_profile = pm.join("agents/w1/profile");
        let w1_knowledge = pm.join("agents/w1/knowledge");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&w1_profile).unwrap();
        fs::create_dir_all(&w1_knowledge).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let state_canon = fs::canonicalize(&state).unwrap();
        let pm_canon = fs::canonicalize(&pm).unwrap();
        let repo_cwd = fs::canonicalize(&repo).unwrap();
        std::os::unix::fs::symlink(pm_canon.join("agents/w1/profile"), repo_cwd.join("prof"))
            .unwrap();

        // 1. +refspec force-pushes are Class::Never and refused for workers.
        for cmd in [
            "git push origin +main",
            "git push origin +HEAD:refs/heads/main",
        ] {
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            assert!(
                never_why(&argv, &repo_cwd, &[state_canon.as_path()]).is_some(),
                "expected never_why for `{cmd}`"
            );
            assert!(
                !matches!(
                    classify(&argv, &repo_cwd, &[], &[state_canon.as_path()]),
                    Class::Requestable { .. } | Class::Allowlisted
                ),
                "expected `{cmd}` not to be requestable or allowlisted"
            );
            let err = evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-plus-push",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &repo_cwd,
                &[state_canon.as_path()],
                1_020,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("force-push"), "{cmd}: {err}");
        }

        // 2. Non-Bash tool smuggling (bash/git/command fields) is refused.
        for (tool, input) in [
            (
                "bash",
                serde_json::json!({"command": "git push --force origin main"}),
            ),
            (
                "git",
                serde_json::json!({"command": "push --force origin main"}),
            ),
            (
                "/usr/bin/git",
                serde_json::json!({"command": "push --force origin main"}),
            ),
            (
                "Write",
                serde_json::json!({"file_path": "a.txt", "command": "git push --force"}),
            ),
        ] {
            evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-smuggle",
                tool,
                &input,
                "smuggle",
                &repo_cwd,
                &[state_canon.as_path()],
                1_021,
            )
            .expect_err(&format!("expected refusal for tool={tool} input={input}"));
        }

        // Even a planted multi-word argv shell rule or request fails closed.
        let smuggled_rule = Rule {
            id: "wr-smuggle".into(),
            effect: Effect::Allow,
            scope: Scope::Exact,
            argv: vec!["git".into(), "push --force origin main".into()],
            tail: vec![],
            cwd: repo_cwd.to_string_lossy().into_owned(),
            by: "operator".into(),
            at: 1_021,
        };
        write_rules(&pm_canon, "w1", &[smuggled_rule]).unwrap();
        assert!(live_rules(&pm_canon, "w1", &[state_canon.as_path()])
            .unwrap()
            .is_empty());
        let mut doc = Doc::default();
        doc.requests.push(Request {
            id: "mp-smuggle".into(),
            argv: vec!["git".into(), "push --force origin main".into()],
            cwd: repo_cwd.to_string_lossy().into_owned(),
            reason: "planted".into(),
            requested_by: "w1".into(),
            created: 1_000,
            expires_at: 1_000 + TTL_SECS,
            status: "pending".into(),
            risk: Risk::High,
            decision: String::new(),
            decision_label: String::new(),
        });
        save_doc(&state_canon, &doc).unwrap();
        allow_once(
            &state_canon,
            "mp-smuggle",
            &[],
            &[state_canon.as_path()],
            1_022,
        )
        .expect_err("allow_once must refuse planted multi-word shell argv");
        always_rule(
            &state_canon,
            "mp-smuggle",
            Scope::Exact,
            &[],
            &[],
            &[state_canon.as_path()],
            1_022,
        )
        .expect_err("always_rule must refuse planted multi-word shell argv");

        // 3. Deeper descendants under pm/agents/** are refused for Write and Bash, including via symlinks.
        let nested_abs = pm_canon.join("agents/w1/profile/note.md");
        for target in [
            nested_abs.to_string_lossy().into_owned(),
            "prof/note.md".into(),
        ] {
            let err_w = evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-nested-write",
                "Write",
                &serde_json::json!({"file_path": target, "content": "x"}),
                &target,
                &repo_cwd,
                &[state_canon.as_path()],
                1_023,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err_w.contains("never requestable"),
                "Write {target}: {err_w}"
            );

            let cmd = format!("cat {target}");
            let err_b = evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-nested-bash",
                "Bash",
                &serde_json::json!({"command": cmd}),
                &cmd,
                &repo_cwd,
                &[state_canon.as_path()],
                1_024,
            )
            .unwrap_err()
            .to_string();
            assert!(
                err_b.contains("never requestable"),
                "Bash {target}: {err_b}"
            );
        }

        // Legitimate `cadence wiki` knowledge paths under pm_dir stay requestable (gated by wiki ACLs).
        let eval_wiki = evaluate_worker(
            &state_canon,
            Some(pm_canon.as_path()),
            "w1",
            "h-wiki-ok",
            "Bash",
            &serde_json::json!({"command": "cadence wiki write agents/w1/knowledge/note.md"}),
            "cadence wiki write agents/w1/knowledge/note.md",
            &pm_canon,
            &[state_canon.as_path()],
            1_025,
        )
        .unwrap();
        assert!(matches!(eval_wiki, WorkerEval::Pending(_)));
    }

    #[test]
    fn interleaved_flags_tilde_wrappers_and_deep_inputs_are_never() {
        let dir = tmp();
        let state = dir.path().join("state");
        let pm = dir.path().join("pm");
        let repo = dir.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(pm.join("agents/w1")).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let state_canon = fs::canonicalize(&state).unwrap();
        let pm_canon = fs::canonicalize(&pm).unwrap();
        let repo_cwd = fs::canonicalize(&repo).unwrap();

        // 1. Interleaved global/named flags never bypass Class::Never.
        for cmd in [
            "cadence audit --org fav-limited approve --pr 1 --head 0123456789abcdef0123456789abcdef01234567 --source ok",
            "cadence audit --wake-timeout 10 approve --pr 1 --head 0123456789abcdef0123456789abcdef01234567 --source ok",
            "cadence master --wake-timeout 10 allow-once mp-1",
            "cadence daemon --wake-timeout 10 restart",
            "cadence master edit --file rep.md SOUL.md",
        ] {
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            assert!(
                matches!(
                    classify(&argv, &repo_cwd, &[], &[state_canon.as_path()]),
                    Class::Never { .. }
                ),
                "expected Class::Never for `{cmd}`"
            );
            let err = evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-flag-never",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &repo_cwd,
                &[state_canon.as_path()],
                1_030,
            )
            .unwrap_err()
            .to_string();
            assert!(err.contains("never requestable"), "{cmd}: {err}");
        }

        // 2. Tilde paths in Bash or tool input fail closed.
        for cmd in [
            "cat ~/pm/agents/w1/profile/secret.md",
            "dd if=~/pm/agents/w1/memory/x of=/tmp/out",
        ] {
            evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-tilde-bash",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &repo_cwd,
                &[state_canon.as_path()],
                1_031,
            )
            .expect_err(&format!("expected tilde refusal for `{cmd}`"));
        }
        evaluate_worker(
            &state_canon,
            Some(pm_canon.as_path()),
            "w1",
            "h-tilde-write",
            "Write",
            &serde_json::json!({"file_path": "~/pm/agents/w1/profile/secret.md", "content": "x"}),
            "~/pm/agents/w1/profile/secret.md",
            &repo_cwd,
            &[state_canon.as_path()],
            1_031,
        )
        .expect_err("expected tilde refusal on Write");

        // 3. Wrapper prefixes never smuggle Class::Never commands past worker evaluation or live rules.
        for cmd in [
            "env git push --force origin main",
            "timeout 30 git push origin +main",
            "nice -n 5 gh pr merge 123",
            "rtk proxy cadence update",
            "unknownwrap git push --force origin main",
            "unknownwrap cadence --org fav-limited audit approve --pr 1 --head 0123456789abcdef0123456789abcdef01234567 --source ok",
        ] {
            let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
            let rule = Rule {
                id: "wr-wrap".into(),
                effect: Effect::Allow,
                scope: Scope::Exact,
                argv,
                tail: vec![],
                cwd: repo_cwd.to_string_lossy().into_owned(),
                by: "operator".into(),
                at: 1_032,
            };
            write_rules(&pm_canon, "w1", &[rule]).unwrap();
            assert!(
                live_rules(&pm_canon, "w1", &[state_canon.as_path()])
                    .unwrap()
                    .is_empty(),
                "wrapped never command must not produce a live rule: `{cmd}`"
            );
            evaluate_worker(
                &state_canon,
                Some(pm_canon.as_path()),
                "w1",
                "h-wrap",
                "Bash",
                &serde_json::json!({"command": cmd}),
                cmd,
                &repo_cwd,
                &[state_canon.as_path()],
                1_032,
            )
            .expect_err(&format!("expected wrapper refusal for `{cmd}`"));
        }
        write_rules(&pm_canon, "w1", &[]).unwrap();

        // 4. Nested arrays and excessive input nesting fail closed, while prose Write bodies stay requestable.
        evaluate_worker(
            &state_canon,
            Some(pm_canon.as_path()),
            "w1",
            "h-arr-soul",
            "mcp__fs__read_many",
            &serde_json::json!({"paths": ["agents/w1/SOUL.md"]}),
            "read_many",
            &repo_cwd,
            &[state_canon.as_path()],
            1_033,
        )
        .expect_err("array element touching protected SOUL.md must be refused");

        evaluate_worker(
            &state_canon,
            Some(pm_canon.as_path()),
            "w1",
            "h-deep-json",
            "mcp__fs__read_many",
            &serde_json::json!({"a": {"b": {"c": {"d": {"e": {"f": "x"}}}}}}),
            "deep",
            &repo_cwd,
            &[state_canon.as_path()],
            1_033,
        )
        .expect_err("deep nesting >5 must fail closed");

        let ok_prose = evaluate_worker(
            &state_canon,
            Some(pm_canon.as_path()),
            "w1",
            "h-prose-write",
            "Write",
            &serde_json::json!({
                "file_path": "ok.md",
                "content": "Stored in ~/.local/state/cadence and agents/master/SOUL.md",
            }),
            "ok.md",
            &repo_cwd,
            &[state_canon.as_path()],
            1_034,
        )
        .unwrap();
        assert!(matches!(ok_prose, WorkerEval::Pending(_)));
    }
}
