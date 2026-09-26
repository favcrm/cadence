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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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

fn alias_ok(alias: &str) -> Result<()> {
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

fn load_rules(pm_dir: &Path, alias: &str) -> Result<Vec<Rule>> {
    let path = rules_file(pm_dir, alias)?;
    if !path.exists() {
        return Ok(vec![]);
    }
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(
            "agents/master/permissions.yaml is a symlink — refusing to read it",
        ));
    }
    let text = fs::read_to_string(&path)?;
    let file: RulesFile = serde_yaml::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "agents/master/permissions.yaml is unreadable ({e}) — refusing to guess"
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
        if want.len() < 1 {
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

fn never_why(argv: &[String], cwd: &Path, state_dirs: &[&Path]) -> Option<&'static str> {
    let tokens: Vec<&str> = argv.iter().map(|s| tool_name(s)).collect();
    let has = |w: &str| tokens.iter().any(|t| *t == w);
    let cadence = is_cadence(argv.first().map(String::as_str).unwrap_or(""));
    if has("merge") && (cadence || tokens.first() == Some(&"git")) {
        return Some("merge is never requestable");
    }
    if cadence && has("rollout") {
        return Some("rollout is never requestable");
    }
    if cadence && tokens.iter().any(|t| t.starts_with("secret")) {
        return Some("secret commands are never requestable");
    }
    if cadence
        && tokens.windows(2).any(|w| {
            (w[0] == "daemon" && matches!(w[1], "restart" | "stop"))
                || (w[0] == "master"
                    && matches!(
                        w[1],
                        "allow-once" | "always-allow" | "reject" | "revoke-permission"
                    ))
        })
    {
        return Some("daemon restart and permission decisions are never requestable");
    }
    if cadence
        && tokens
            .windows(2)
            .any(|w| w[0] == "audit" && w[1] == "approve")
    {
        return Some("audit approve is never requestable");
    }
    if argv.iter().any(|a| protected_file(a)) {
        return Some("the master's SOUL.md, AGENT.md and permissions.yaml are never requestable");
    }
    if touches_state(argv, cwd, state_dirs) {
        return Some("the daemon state dir is never requestable");
    }
    None
}

fn protected_file(arg: &str) -> bool {
    let path = arg.replace('\\', "/");
    path.contains("agents/master/SOUL.md")
        || path.contains("agents/master/AGENT.md")
        || path.contains("agents/master/permissions.yaml")
        || path.ends_with("/permissions.yaml") && path.contains("agents/master")
}

fn touches_state(argv: &[String], cwd: &Path, state_dirs: &[&Path]) -> bool {
    // The master's cwd is inside the state dir (`master/cwd`). That
    // fact is not a path the command touches — only arguments that
    // resolve into a state dir are.
    let mut probes = Vec::new();
    for arg in argv {
        if arg.starts_with('/') || arg.starts_with('.') {
            probes.push(if Path::new(arg).is_absolute() {
                PathBuf::from(arg)
            } else {
                cwd.join(arg)
            });
        }
        if arg.contains(".local/state/cadence") || arg.contains(".local/share/cadence") {
            return true;
        }
    }
    for probe in probes {
        let canon = fs::canonicalize(&probe).unwrap_or(probe);
        for dir in state_dirs {
            let root = fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            if canon == root || canon.starts_with(&root) {
                return true;
            }
        }
    }
    false
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
    if allowlisted(argv) {
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
        Class::Allowlisted => {
            return Err(Error::rejected(
                "that command is already allowlisted — run it, do not ask",
            ))
        }
        Class::Never { why } => {
            return Err(Error::rejected(format!(
                "{why} — it cannot be approved and no permission rule can cover it"
            )))
        }
        Class::IllFormed { why } => return Err(Error::rejected(why)),
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

/// Allow the request's exact argv and cwd once.
pub fn allow_once(state_dir: &Path, id: &str, now: i64) -> Result<Request> {
    with_doc(state_dir, now, |doc| {
        let req = pending(doc, id, now)?;
        req.status = "allowed".into();
        req.decision = "allow_once".into();
        req.decision_label = format!("Allowed once by operator {}", decision_hm(now));
        let req = req.clone();
        doc.grants.push(Grant {
            id: fresh_id("mg"),
            request_id: req.id.clone(),
            argv: req.argv.clone(),
            cwd: req.cwd.clone(),
            uses_left: 1,
            expires_at: now + TTL_SECS,
        });
        Ok(req)
    })
}

/// A wildcard is only legal as the last character of an argument, and
/// never in the verb head.
pub fn validate_pattern(pat: &str) -> Result<()> {
    if !pat.contains('*') {
        return if arg_ok(pat) {
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
    if stem.is_empty() || arg_ok(stem) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "pattern '{pat}' has characters a permission rule cannot store"
        )))
    }
}

fn pattern_matches(pat: &str, arg: &str) -> bool {
    match pat.strip_suffix('*') {
        Some(stem) => arg.starts_with(stem),
        None => pat == arg,
    }
}

/// Reject a rule that would name a different verb or a mid-argument
/// wildcard. `head` is the literal verb; `tail` is the argument patterns.
pub fn check_rule_shape(head: &[String], tail: &[String], scope: &Scope) -> Result<()> {
    match scope {
        Scope::Exact => {
            if head.len() < 2 || !tail.is_empty() {
                return Err(Error::rejected(
                    "an exact rule is the full command, with no wildcard tail",
                ));
            }
            if head.iter().any(|a| a.contains('*')) {
                return Err(Error::rejected("an exact rule cannot contain a wildcard"));
            }
        }
        Scope::Prefix => {
            if head.len() < 2 {
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
pub fn always_rule(
    state_dir: &Path,
    id: &str,
    scope: Scope,
    tail: &[String],
    now: i64,
) -> Result<(Request, Rule)> {
    with_doc(state_dir, now, |doc| {
        let req = pending(doc, id, now)?.clone();
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

pub fn write_rules(pm_dir: &Path, alias: &str, rules: &[Rule]) -> Result<PathBuf> {
    let path = rules_file(pm_dir, alias)?;
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(
            "agents/master/permissions.yaml is a symlink — refusing to write it",
        ));
    }
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
    Ok(matches!(
        decide_use(state_dir, pm_dir, argv, cwd, checkouts, state_dirs, now, false)?,
        Some(_)
    ))
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
        Some(dir) => load_rules(dir, crate::master::ALIAS)?,
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
    if let Some(grant) = doc
        .grants
        .iter_mut()
        .find(|g| g.uses_left > 0 && g.expires_at > now && g.argv == argv && g.cwd == cwd_s)
    {
        let request_id = grant.request_id.clone();
        if consume {
            grant.uses_left = 0;
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

pub fn pending_requests(state_dir: &Path, now: i64) -> Result<Vec<Request>> {
    let lock = dir_lock(state_dir);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let mut doc = load_doc(state_dir)?;
    expire(&mut doc, now);
    save_doc(state_dir, &doc)?;
    Ok(doc
        .requests
        .into_iter()
        .filter(|r| r.status == "pending")
        .collect())
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
pub fn suggest_tail(argv: &[String]) -> Option<Vec<String>> {
    if argv.len() < 3 {
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
    Some(vec![format!("{parent}/*")])
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
        "prefix": suggest_tail(&req.argv),
    })
}

pub fn rule_json(rule: &Rule) -> Value {
    json!({
        "id": rule.id,
        "effect": match rule.effect { Effect::Allow => "allow", Effect::Deny => "deny" },
        "scope": match rule.scope { Scope::Exact => "exact", Scope::Prefix => "prefix" },
        "argv": rule.argv,
        "tail": rule.tail,
        "cwd": rule.cwd,
        "by": rule.by,
        "at": rule.at,
    })
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
        let err = allow_once(dir.path(), "mp-forged", 1_000)
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
            "new".into(),
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
        allow_once(dir.path(), &req.id, 1_000).unwrap();
        let other = vec![
            "cadence".into(),
            "issue".into(),
            "new".into(),
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
        allow_once(dir.path(), &req.id, 2_000).unwrap();
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
            "new".into(),
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
        allow_once(dir.path(), &req.id, 5_000).unwrap();
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
            "put".into(),
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
            "put".into(),
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
            "new".into(),
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
        let (_r, rule) = always_rule(dir.path(), &req.id, Scope::Exact, &[], 4_000).unwrap();
        write_rules(&pm, "master", &[rule.clone()]).unwrap();
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
            "new".into(),
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
        let err = allow_once(dir.path(), &req.id, 100 + TTL_SECS)
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
            &["cadence".into(), "issue".into(), "new".into(), "T".into()],
            &cwd,
            "need a ticket",
            "master",
            &[],
            &[],
            1_000,
        )
        .unwrap();
        allow_once(dir.path(), &req.id, 1_000).unwrap();
        let again = allow_once(dir.path(), &req.id, 1_001)
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
}
