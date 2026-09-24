//! `cadence report` — intake: a question, feedback, idea or bug
//! becomes a tracker issue with context, instead of dying in a
//! terminal scrollback. Routing is by kind, not by cwd: `question`,
//! `feedback` and `bug` are about cadence itself and file into the
//! `cadence` project from wherever the operator stands; `idea` is
//! about the project being worked on and resolves like `issue new`
//! (cwd repo, `CADENCE_PROJECT`, `--project` — which always wins).
//! `--issue` files a comment on an existing issue instead.
//!
//! Every report also (a) sends one line to the project's PM inbox
//! when one is resolvable — the reporter's `upstream` first, then the
//! project's `team.yaml` `roles.pm.alias` — and (b) surfaces as an
//! Overview `needs_me` row of kind `intake` while the issue sits in
//! `backlog` (see `src/overview.rs`). The report never fails because
//! notification did — the issue file is the durable record.
//!
//! Hygiene: bodies are capped ([`BODY_MAX`] bytes, [`TITLE_MAX`]
//! chars on the title), control characters other than `\n`/`\t` are
//! stripped before anything is stored, and the PM line goes through
//! the same control-free contract as `kickoff_body`. Credential
//! hygiene is best-effort, not absolute — argv-shaped context fields
//! (`cwd`, `repo`, `remote`, `actor`) still pass through the shared
//! `doctor::host::redact_argv` scrubber, while free prose gets
//! `scrub_body`: `key: value` / `key = value` / `key is value`
//! forms, `--flag value` and `-u user:pass`, `Authorization:` /
//! `Proxy-Authorization:` header lines (the scheme word must not
//! shield the credential), URI query parameters, PEM blocks, and the
//! per-token credential shapes. Prose has no flag convention — a
//! bare `hunter2` after no keyword survives; do not paste secrets.
//! Before any of that, the raw body goes through the CAD-109 secret
//! scan (`crate::secret::guard`): a blocking finding refuses the report
//! with the rule named and nothing is filed.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::client;
use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, model, project, task_report, time, write, Pm};
use crate::proc::run_bounded;

/// Stored body cap — a paste bigger than this is refused, not
/// truncated silently, because a partial stack trace reads like a
/// complete one.
pub const BODY_MAX: usize = 32 * 1024;
/// Title cap — the first line is the issue title, shown on every
/// board row and PM heads-up.
const TITLE_MAX: usize = 200;
/// `needs_me` shows at most this many intake rows plus a summary row.
pub const NEEDS_ME_CAP: usize = 10;
const REDACTED: &str = "[REDACTED]";
/// `git rev-parse` inside `context_block` must never wedge the verb.
const GIT_TIMEOUT: Duration = Duration::from_secs(15);

/// `report`'s kinds — the routing key. `value_enum` keeps clap in
/// sync; `as_str` is the stored tag/comment kind.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Kind {
    Question,
    Feedback,
    Idea,
    Bug,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Feedback => "feedback",
            Kind::Idea => "idea",
            Kind::Bug => "bug",
        }
    }
    /// Kinds that concern cadence itself — always the `cadence`
    /// project regardless of cwd.
    fn about_cadence(self) -> bool {
        matches!(self, Kind::Question | Kind::Feedback | Kind::Bug)
    }
    fn default_priority(self) -> &'static str {
        match self {
            Kind::Bug => "P2",
            _ => "P3",
        }
    }
}

/// C0/C1/DEL control characters never reach the tracker or a
/// terminal — `\n` and `\t` survive because the body needs its line
/// structure and indentation; `\r` and everything else go.
fn strip_controls(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// One line, control-free — the same contract `kickoff_body`'s
/// `clean()` enforces on dispatch text that lands on a pty.
fn clean_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A credential-shaped token inside an argv-shaped field (cwd, repo,
/// remote, actor, daemon build) is replaced before it reaches the
/// tracker — the shared process-list scrubber.
fn scrub(text: &str) -> String {
    redact_argv(&text.split_whitespace().collect::<Vec<_>>())
}

/// One whitespace-run-delimited token paired with the whitespace that
/// follows it — preserved byte-for-byte so line structure, blank
/// lines and indentation survive scrubbing.
fn tokens(s: &str) -> Vec<(&str, &str)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (start, c.is_whitespace()) {
            (None, false) => start = Some(i),
            (Some(st), true) => {
                spans.push((st, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(st) = start {
        spans.push((st, s.len()));
    }
    spans
        .iter()
        .enumerate()
        .map(|(k, (a, b))| {
            let end = spans.get(k + 1).map(|(na, _)| *na).unwrap_or(s.len());
            (&s[*a..*b], &s[*b..end])
        })
        .collect()
}

/// Does `name` name a credential? Probed through the canonical word
/// list by offering the shared scrubber a synthetic `--name=x` flag:
/// the mask fires iff `secret_name` holds, so prose `key:`/`key=`
/// forms share the argv vocabulary instead of duplicating it.
fn name_is_secret(name: &str) -> bool {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return false;
    }
    let probe = format!("--{name}=x");
    redact_argv(std::slice::from_ref(&probe)) != probe
}

/// A standalone credential shape — ghp_/sk_/AKIA/JWT, 32+ entropy,
/// `scheme://user:pass@host` userinfo, `-u<user:pass>` glued shorts —
/// probed per token through the shared scrubber.
fn standalone_secret(tok: &str) -> bool {
    redact_argv(&[tok.to_string()]) != tok
}

/// `-x`/`--name` carrying no inline `=` whose name is
/// credential-bearing (`--password`, `-p`, `-u`, `-a` — argv parity).
fn secret_flag(tok: &str) -> bool {
    let bare = tok.trim_matches(|c| c == '"' || c == '\'');
    if !bare.starts_with('-') || bare.len() < 2 || bare.contains('=') {
        return false;
    }
    let name = bare.trim_start_matches('-');
    name_is_secret(name) || (!bare.starts_with("--") && matches!(name, "p" | "a" | "u"))
}

/// `Authorization:`/`Proxy-Authorization:` — the scheme word
/// (`Basic`/`Bearer`) must not shield the credential after it, so the
/// rest of the line is masked whole.
fn auth_header(tok: &str) -> bool {
    let bare = tok.trim_matches(|c| c == '"' || c == '\'');
    let head = bare
        .split([':', '='])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(head.as_str(), "authorization" | "proxy-authorization")
}

/// Find an unescaped closing quote. Values are prose, not shell input, but
/// honoring a backslash keeps an escaped quote from ending the mask early.
fn quote_end(s: &str, q: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == q {
            return Some(i);
        }
    }
    None
}

/// Consume tokens after an already-emitted opening quote. The secret value
/// is collapsed to one marker; only the closing quote, token suffix and the
/// whitespace after the closing token are retained. A missing close carries
/// the quote state into the next line.
fn consume_quote_tail(
    toks: &[(&str, &str)],
    start: usize,
    q: char,
    out: &mut String,
) -> (usize, bool) {
    let mut j = start;
    while j < toks.len() {
        let (tok, ws) = toks[j];
        if let Some(pos) = quote_end(tok, q) {
            out.push(q);
            out.push_str(&tok[pos + q.len_utf8()..]);
            out.push_str(ws);
            return (j + 1, true);
        }
        j += 1;
    }
    (j, false)
}

/// Emit a quoted secret value beginning at `open_at` in `toks[i].0`.
/// `open_at` is zero for a separate value (`--password "secret"`) and
/// points after `=`/`:` for a glued value (`--password="secret words"`).
fn emit_quoted_value(
    toks: &[(&str, &str)],
    i: usize,
    open_at: usize,
    q: char,
    out: &mut String,
    open_quote: &mut Option<char>,
) -> usize {
    let (tok, ws) = toks[i];
    out.push_str(&tok[..open_at]);
    out.push(q);
    out.push_str(REDACTED);
    let value_start = open_at + q.len_utf8();
    if let Some(pos) = quote_end(&tok[value_start..], q) {
        let close = value_start + pos;
        out.push(q);
        out.push_str(&tok[close + q.len_utf8()..]);
        out.push_str(ws);
        return i + 1;
    }
    let (next, closed) = consume_quote_tail(toks, i + 1, q, out);
    if !closed {
        *open_quote = Some(q);
    }
    next
}

/// Emit `[REDACTED]` for `toks[i]` (trailing whitespace kept) and carry an
/// unclosed quote into later lines. `--password "correct horse battery"`
/// collapses the span to one marker inside the quotes.
fn eat_value_at(
    toks: &[(&str, &str)],
    i: usize,
    out: &mut String,
    open_quote: &mut Option<char>,
) -> usize {
    let (tok, ws) = toks[i];
    if let Some(q) = tok.chars().next().filter(|c| matches!(c, '"' | '\'')) {
        return emit_quoted_value(toks, i, 0, q, out, open_quote);
    }
    out.push_str(REDACTED);
    out.push_str(ws);
    i + 1
}

/// `?name=value&…` inside a token — mask each query parameter whose
/// name is credential-bearing or whose value is a credential shape.
fn mask_query_params(tok: &str) -> String {
    let Some(q) = tok.find('?') else {
        return tok.to_string();
    };
    let (head, query) = tok.split_at(q + 1);
    let mut out = String::from(head);
    for pair in query.split('&') {
        if !out.ends_with(['?', '&']) {
            out.push('&');
        }
        match pair.split_once('=') {
            Some((name, val)) if name_is_secret(name) || standalone_secret(val) => {
                out.push_str(name);
                out.push('=');
                out.push_str(REDACTED);
            }
            _ => out.push_str(pair),
        }
    }
    out
}

/// Scrub one line of free prose, whitespace preserved. `carry_eat`
/// carries a pending `key:`/`key=` value across the line boundary so
/// `password:\n  hunter2` still masks `hunter2`; `open_quote` carries a
/// quoted value across the same boundary.
fn scrub_line(line: &str, carry_eat: &mut bool, open_quote: &mut Option<char>, out: &mut String) {
    let toks = tokens(line);
    // Leading whitespace is indentation — verbatim.
    let lead = line
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(line.len());
    out.push_str(&line[..lead]);
    let mut i = 0;
    if let Some(q) = *open_quote {
        let (next, closed) = consume_quote_tail(&toks, i, q, out);
        if !closed {
            return;
        }
        *open_quote = None;
        i = next;
    }
    if *carry_eat && i < toks.len() {
        *carry_eat = false;
        i = eat_value_at(&toks, i, out, open_quote);
    }
    while i < toks.len() {
        let (tok, ws) = toks[i];
        let bare = tok.trim_matches(|c| c == '"' || c == '\'');

        // `Authorization:`/`Proxy-Authorization:` — mask the
        // credential; the scheme word is not secret. A quoted header
        // (`-H "Authorization: …"`) stops the mask at the closing
        // quote so trailing arguments survive; a bare header masks
        // the rest of the line.
        if auth_header(tok) {
            let q = tok.chars().next().filter(|c| matches!(c, '"' | '\''));
            let name_end = bare.find([':', '=']).unwrap_or(bare.len());
            if let Some(q) = q {
                out.push(q);
            }
            out.push_str(&bare[..name_end]);
            out.push_str(": ");
            out.push_str(REDACTED);
            match q {
                Some(q) => {
                    let name_start = q.len_utf8();
                    let value_start = name_start + name_end + 1;
                    if value_start < tok.len() {
                        if let Some(pos) = quote_end(&tok[value_start..], q) {
                            let close = value_start + pos;
                            out.push(q);
                            out.push_str(&tok[close + q.len_utf8()..]);
                            out.push_str(ws);
                            i += 1;
                        } else {
                            let (next, closed) = consume_quote_tail(&toks, i + 1, q, out);
                            if !closed {
                                *open_quote = Some(q);
                            }
                            i = next;
                        }
                    } else {
                        let (next, closed) = consume_quote_tail(&toks, i + 1, q, out);
                        if !closed {
                            *open_quote = Some(q);
                        }
                        i = next;
                    }
                }
                None => break,
            }
            continue;
        }

        // `--password`, `-p`, `-u` — value is the next token (unless
        // that token is itself a secret flag, argv parity).
        if secret_flag(tok) {
            out.push_str(tok);
            out.push_str(ws);
            if toks.get(i + 1).is_some_and(|(n, _)| !secret_flag(n)) {
                i = eat_value_at(&toks, i + 1, out, open_quote);
            } else {
                *carry_eat = i + 1 >= toks.len();
                i += 1;
            }
            continue;
        }

        // URI query parameters — `https://api/x?api_key=abcd1234`.
        if bare.contains('?') && bare.contains('=') {
            let masked = mask_query_params(bare);
            if masked != bare {
                out.push_str(&masked);
                out.push_str(ws);
                i += 1;
                continue;
            }
        }

        // `name=value` / `name:value` glued forms — only when a
        // non-empty value follows the separator; `key=`/`key:` alone
        // fall through to the trailing-separator rule, which eats the
        // next token.
        if let Some(eq) = tok.find('=') {
            let name = tok[..eq].trim_matches(|c| c == '"' || c == '\'');
            if eq + 1 < tok.len() && name_is_secret(name.trim_start_matches('-')) {
                let value_start = eq + 1;
                if let Some(q) = tok[value_start..]
                    .chars()
                    .next()
                    .filter(|c| matches!(c, '"' | '\''))
                {
                    i = emit_quoted_value(&toks, i, value_start, q, out, open_quote);
                } else {
                    out.push_str(&tok[..value_start]);
                    out.push_str(REDACTED);
                    out.push_str(ws);
                    i += 1;
                }
                continue;
            }
        }
        if let Some(c) = tok.find(':') {
            let name = tok[..c].trim_matches(|c| c == '"' || c == '\'');
            if c + 1 < tok.len() && name_is_secret(name) {
                let value_start = c + 1;
                if let Some(q) = tok[value_start..]
                    .chars()
                    .next()
                    .filter(|c| matches!(c, '"' | '\''))
                {
                    i = emit_quoted_value(&toks, i, value_start, q, out, open_quote);
                } else {
                    out.push_str(&tok[..value_start]);
                    out.push_str(REDACTED);
                    out.push_str(ws);
                    i += 1;
                }
                continue;
            }
        }

        // `key:`/`key=` trailing separator — value is the next token.
        if (bare.ends_with(':') || bare.ends_with('='))
            && bare.len() > 1
            && name_is_secret(&bare[..bare.len() - 1])
        {
            out.push_str(tok);
            out.push_str(ws);
            if toks.get(i + 1).is_some_and(|(n, _)| !secret_flag(n)) {
                i = eat_value_at(&toks, i + 1, out, open_quote);
            } else {
                *carry_eat = i + 1 >= toks.len();
                i += 1;
            }
            continue;
        }

        // Bare secret word — `password is hunter2`, `token <shape>`.
        // A connector (`is`, `=`, `:`) makes the following token the
        // value unconditionally; without one, only a credential-shaped
        // neighbour is masked — `the key point` stays prose.
        if !bare.starts_with('-') && name_is_secret(bare) {
            out.push_str(tok);
            out.push_str(ws);
            if let Some((ntok, nws)) = toks.get(i + 1) {
                let nlow = ntok.trim_matches('"').to_ascii_lowercase();
                if matches!(nlow.as_str(), "is" | "=" | ":") {
                    out.push_str(ntok);
                    out.push_str(nws);
                    if toks.get(i + 2).is_some_and(|(n, _)| !secret_flag(n)) {
                        i = eat_value_at(&toks, i + 2, out, open_quote);
                    } else {
                        *carry_eat = i + 2 >= toks.len();
                        i += 2;
                    }
                    continue;
                }
                if standalone_secret(ntok) {
                    out.push_str(REDACTED);
                    out.push_str(nws);
                    i += 2;
                    continue;
                }
            }
            i += 1;
            continue;
        }

        // Ordinary token — per-token shapes (`ghp_…`, JWTs, in-token
        // `Key: value`, URI userinfo) via the shared scrubber.
        out.push_str(&redact_argv(&[tok.to_string()]));
        out.push_str(ws);
        i += 1;
    }
}

/// Free-prose credential pass — applied to the report body. Line
/// structure is preserved exactly; PEM blocks mask line-by-line.
fn scrub_body(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_pem = false;
    let mut carry_eat = false;
    let mut open_quote = None;
    for chunk in text.split_inclusive('\n') {
        let (line, eol) = match chunk.strip_suffix('\n') {
            Some(l) => (l, "\n"),
            None => (chunk, ""),
        };
        let trimmed = line.trim_start();
        if trimmed.starts_with("-----BEGIN ") {
            in_pem = true;
        }
        if in_pem {
            out.push_str(REDACTED);
            out.push_str(eol);
            if trimmed.starts_with("-----END ") {
                in_pem = false;
            }
            continue;
        }
        scrub_line(line, &mut carry_eat, &mut open_quote, &mut out);
        out.push_str(eol);
    }
    out
}

/// `s` truncated to `max` chars on a char boundary.
fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// The routing decision: which project the report files into.
fn target_project(pm: &Pm, kind: Kind, flag: Option<&str>, cwd: &Path) -> Result<project::Project> {
    // --project always wins, whatever the kind.
    if let Some(name) = flag {
        return project::resolve(&pm.dir, Some(name), cwd);
    }
    if kind.about_cadence() {
        return project::list(&pm.dir)?
            .into_iter()
            .find(|p| p.key == "cadence")
            .ok_or_else(|| {
                Error::rejected(
                    "No 'cadence' project on this tracker — register one with \
                     `cadence issue project add cadence --prefix <P>` or pass --project",
                )
            });
    }
    // `idea` — the cwd's project; resolve()'s error already names
    // --project and lists the known keys.
    project::resolve(&pm.dir, None, cwd)
}

/// Who filed it: the cadence alias inside a pane, else the OS user.
fn actor_of() -> String {
    std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// Bounded `cmd` stdout — a wedged `git` must not hang the verb.
fn out(cmd: &mut std::process::Command) -> Option<String> {
    let o = run_bounded(cmd, GIT_TIMEOUT).ok()?;
    if !o.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The automatic context block — everything an issue needs to locate
/// the reporter's world without another round-trip. Daemon build comes
/// from `daemon_info`; unreachable records that fact, never fails.
fn context_block(state_dir: &Path, cwd: &Path) -> String {
    let mut lines = vec![format!("- actor: {}", scrub(&actor_of()))];
    lines.push(format!("- cwd: {}", scrub(&cwd.to_string_lossy())));
    if let Some((root, remote)) = project::repo_identity(cwd) {
        let branch = out(std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "--abbrev-ref", "HEAD"]))
        .unwrap_or_default();
        let mut repo = format!("- repo: {}", scrub(&root.to_string_lossy()));
        if !branch.is_empty() {
            repo.push_str(&format!(" (branch {})", scrub(&branch)));
        }
        if let Some(r) = remote {
            repo.push_str(&format!(" remote {}", scrub(&r)));
        }
        lines.push(repo);
    }
    lines.push(format!("- cadence: {}", env!("CARGO_PKG_VERSION")));
    let daemon = client::rpc(state_dir, "daemon_info", json!({}))
        .ok()
        .and_then(|i| {
            i["build_commit"]
                .as_str()
                .map(str::to_string)
                .or_else(|| Some("unknown".to_string()))
        })
        .unwrap_or_else(|| "unreachable".to_string());
    lines.push(format!("- daemon: {}", scrub(&daemon)));
    format!("## Report context\n\n{}\n", lines.join("\n"))
}

/// Resolve the PM inbox to notify, best-effort: the reporter's
/// `upstream` when reporting from inside a pane, else the project's
/// `team.yaml` `roles.pm.alias` (ADR 0001), else none — a missing PM
/// never blocks the report.
fn pm_inbox(pm: &Pm, project: &project::Project, state_dir: &Path) -> Option<String> {
    if let Ok(alias) = std::env::var("CADENCE_ALIAS") {
        if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
            if let Some(up) = show["agent"]["params"]["upstream"].as_str() {
                if !up.is_empty() {
                    return Some(up.to_string());
                }
            }
        }
    }
    let team = pm.dir.join(&project.key).join("team.yaml");
    if let Ok(text) = std::fs::read_to_string(&team) {
        let y: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
        return y["roles"]["pm"]["alias"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

/// `agent_send`'s message-id charset is lowercase letters, digits and
/// hyphens — the idempotency key is built inside it so a retried send
/// dedupes instead of double-notifying.
fn msg_key(parts: &[&str]) -> String {
    let mut s = String::from("report-notify");
    for p in parts {
        for c in p.chars() {
            let c = c.to_ascii_lowercase();
            let c = if c.is_ascii_alphanumeric() { c } else { '-' };
            if !(c == '-' && s.ends_with('-')) {
                s.push(c);
            }
        }
        if !s.ends_with('-') {
            s.push('-');
        }
    }
    cap_chars(s.trim_end_matches('-'), 64)
}

/// Best-effort one-line heads-up to the PM inbox. `key` is the
/// idempotency key — stable per issue/comment so a retried send
/// dedupes instead of double-notifying. The return value is what the
/// caller records — failures are data, never report errors. The line
/// itself is control-free (`clean_line`), the same contract
/// `kickoff_body` keeps for pty-bound text.
fn notify_pm(
    state_dir: &Path,
    inbox: Option<&str>,
    line: &str,
    reply_to: Option<&str>,
    msg_key: &str,
) -> Value {
    let Some(alias) = inbox else {
        return json!(null);
    };
    match client::rpc(
        state_dir,
        "agent_send",
        json!({"alias": alias, "text": clean_line(line), "reply_to": reply_to,
               "message": msg_key}),
    ) {
        Ok(r) => {
            json!({"to": alias, "sent": true, "duplicate": r["duplicate"].as_bool()})
        }
        Err(e) => json!({"to": alias, "sent": false, "error": e.to_string()}),
    }
}

/// `cadence report` — file one. `body` is the already-read report
/// text (`-m`, `--file` or stdin); the caller owns stdin/TTY policy.
#[allow(clippy::too_many_arguments)]
pub fn file(
    pm: &Pm,
    kind: Kind,
    project_flag: Option<&str>,
    issue_id: Option<&str>,
    priority: Option<&str>,
    body: &str,
    actor: &str,
    state_dir: &Path,
    cwd: &Path,
) -> Result<Value> {
    if body.len() > BODY_MAX {
        return Err(Error::rejected(format!(
            "Report body exceeds the {} KB cap — trim it or `--issue` it onto an existing issue",
            BODY_MAX / 1024
        )));
    }
    let body = strip_controls(body);
    if body.trim().is_empty() {
        return Err(Error::rejected("Report body is empty — pass -m or --file"));
    }
    // CAD-109: refuse credential-shaped input outright. `scrub_body` below
    // stays as defence in depth for the shapes the scan does not block.
    let secret_warnings = crate::secret::guard("report", &body)?;
    // Scrub the complete body before splitting the title. Pending key/value,
    // PEM and quoted-span state must survive the title/body boundary.
    let body = scrub_body(&body);
    let title = cap_chars(first_line(&body), TITLE_MAX);
    let rest = body
        .split_once('\n')
        .map(|x| x.1)
        .unwrap_or("")
        .trim_matches('\n');
    let context = context_block(state_dir, cwd);
    let reporter = std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty());

    // --issue: the report lands as a comment on an existing issue —
    // no new issue, no routing decision; the issue's own project holds
    // it. The PM heads-up still goes out (keyed on the comment file,
    // so a retried send dedupes). `--issue` is also the retry path for
    // a missed heads-up: it reuses the issue rather than duplicating it.
    if let Some(id) = issue_id {
        let body_text = if rest.is_empty() {
            title.clone()
        } else {
            format!("{title}\n\n{rest}")
        };
        let text = format!("{body_text}\n\n{context}");
        let out = write::add_comment(pm, id, &text, None, Some(kind.as_str()), None, actor)?;
        let (proj, _) = write::issue_dir(pm, id)?;
        let inbox = pm_inbox(pm, &proj, state_dir);
        let comment = out["comment"].as_str().unwrap_or("comment");
        let notified = notify_pm(
            state_dir,
            inbox.as_deref(),
            &format!("{id}: {} comment — {}", kind.as_str(), title),
            reporter.as_deref(),
            &msg_key(&[id, comment]),
        );
        let mut out = out;
        out["kind"] = json!(kind.as_str());
        out["project"] = json!(proj.key);
        out["notified"] = notified;
        return Ok(out);
    }

    let project = target_project(pm, kind, project_flag, cwd)?;
    let priority = priority.unwrap_or_else(|| kind.default_priority());
    model::check_priority(priority)?;
    // `intake`+kind are system vocabulary — `check_tags` exempts them
    // from a project's declared `tags:` allowlist.
    let tags = write::check_tags(&project, &["intake".to_string(), kind.as_str().to_string()])?;

    if title.is_empty() {
        return Err(Error::rejected("Report needs a first line as its title"));
    }
    let issue_body = if rest.is_empty() {
        format!("{title}\n\n{context}")
    } else {
        format!("{title}\n\n{rest}\n\n{context}")
    };

    let _lock = pm.lock()?;
    let id = format!(
        "{}-{}",
        project.prefix,
        write::next_id(&pm.dir.join(&project.key), &project.prefix)?
    );
    let dir = pm.dir.join(&project.key).join(&id);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Issue '{id}' already exists at {}",
            dir.display()
        )));
    }
    let mut front = model::Front::new(&id, &title, &time::iso(time::now_epoch()));
    front.priority = priority.to_string();
    front.tags = tags;
    front.kind = Some(kind.as_str().to_string());
    std::fs::create_dir_all(dir.join("comments"))?;
    std::fs::create_dir_all(dir.join("artifacts"))?;
    write::save_front(&dir, &front, &issue_body)?;
    let issues = board::load_all(&pm.dir, None)?;
    if let Err(e) = write::check_structure(&issues, &id) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    let foreign = match write::commit(
        pm,
        &[dir.join("issue.md")],
        &format!("{id}: report {}", kind.as_str()),
        &[&id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };

    let inbox = pm_inbox(pm, &project, state_dir);
    let notified = notify_pm(
        state_dir,
        inbox.as_deref(),
        &format!("{id}: new {} — {}", kind.as_str(), title),
        reporter.as_deref(),
        &msg_key(&[&id]),
    );
    let mut out = json!({
        "id": id, "project": project.key, "kind": kind.as_str(),
        "priority": priority, "status": "backlog",
        "path": dir, "committed": true, "notified": notified,
    });
    if !foreign.is_empty() {
        out["foreign_files"] = json!(foreign);
    }
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    Ok(out)
}

fn first_line(body: &str) -> &str {
    body.lines().next().unwrap_or("").trim()
}

/// The intake kind — its own frontmatter field since round 2; the
/// tag scan is only a fallback for issues filed before it existed.
fn report_kind(front: &model::Front) -> String {
    front
        .kind
        .clone()
        .or_else(|| front.tags.iter().find(|t| *t != "intake").cloned())
        .unwrap_or_default()
}

/// The `report ls` selection — every field is a repeatable any-of
/// filter; different fields AND (CAD-437's grammar).
#[derive(Clone, Debug, Default)]
pub struct LsFilter {
    /// Intake kinds (`question` `feedback` `idea` `bug`) and
    /// `cadence.report/2` kinds (`done` `question` `blocked` `answer`
    /// `verdict`) — one shared vocabulary.
    pub kinds: Vec<String>,
    /// Intake id, or the ticket a `cadence.report/2` report is filed
    /// on — `--ticket X` is "reports about X".
    pub tickets: Vec<String>,
    /// Intake `actor:` / task-report `agent:`.
    pub agents: Vec<String>,
    pub projects: Vec<String>,
    /// `intake` or `task`.
    pub sources: Vec<String>,
    /// Only strictly-open rows (open intake, unanswered questions).
    pub open: bool,
    /// Everything, including resolved rows (closed intake, answered
    /// questions). Conflicts with `open`.
    pub all: bool,
    /// `--sort` spec (default `-at` — newest first).
    pub sort: Option<String>,
    pub limit: Option<usize>,
    /// `--fields` — keep only these keys per row.
    pub fields: Vec<String>,
}

/// The `--kind` vocabulary — intake kinds plus `cadence.report/2`
/// kinds.
pub const LS_KINDS: &[&str] = &[
    "question", "feedback", "idea", "bug", "done", "blocked", "answer", "verdict",
];
/// The `--source` vocabulary.
pub const LS_SOURCES: &[&str] = &["intake", "task"];

/// The `- actor:` line inside an intake issue's `## Report context`.
fn intake_actor(body: &str) -> Option<String> {
    let ctx = body.split("## Report context").nth(1)?;
    ctx.lines()
        .find_map(|l| l.trim().strip_prefix("- actor:").map(str::trim))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `cadence report ls` — the report ledger: intake issues (questions,
/// feedback, ideas, bugs filed through `report`) plus `cadence.
/// report/2` task reports under each ticket's `reports/` folder.
///
/// Bare `report ls` lists what needs attention — open intake and
/// unanswered questions (the pre-CAD-437 behaviour plus open task
/// questions). With filter flags it queries the ledger: rows that are
/// open plus the `done`/`blocked`/`answer`/`verdict` records, which
/// carry `open: null` and are never "resolved". `--open` keeps only
/// strictly-open rows; `--all` adds the resolved ones (closed intake,
/// answered questions). Newest first.
pub fn ls(pm: &Pm, f: &LsFilter) -> Result<Value> {
    crate::filter::check_set("kind", &f.kinds, LS_KINDS)?;
    crate::filter::check_set("source", &f.sources, LS_SOURCES)?;
    for t in &f.tickets {
        model::check_id(t)?;
    }
    for p in &f.projects {
        model::check_key(p)?;
        if !project::list(&pm.dir)?.iter().any(|pr| &pr.key == p) {
            return Err(project::unknown_project(p, &pm.dir));
        }
    }
    if f.open && f.all {
        return Err(Error::rejected("--open and --all conflict"));
    }
    let issues = board::load_all(&pm.dir, None)?;
    let views = board::views(&pm.config.notes_dir(), issues);
    let mut rows: Vec<Value> = vec![];
    for v in &views {
        let front = &v.issue.front;
        if front.tags.iter().any(|t| t == "intake") {
            let open = !matches!(v.status.as_str(), "done" | "dropped");
            rows.push(json!({
                "id": front.id,
                "name": front.id,
                "source": "intake",
                "kind": report_kind(front),
                "ticket": front.id,
                "project": v.issue.project,
                "agent": intake_actor(&v.issue.body),
                "open": open,
                "status": v.status,
                "priority": front.priority,
                "title": front.title,
                "owner": front.owner,
                "at": front.created,
                // Pre-CAD-437 rows were keyed `created` — kept as an
                // alias of `at` for consumers written against that.
                "created": front.created,
            }));
        }
        for r in task_report::list(&v.issue.dir, &front.id) {
            let mut row = json!({
                "id": r["name"],
                "name": r["name"],
                "path": r["path"],
                "source": "task",
                "kind": r["kind"],
                "ticket": r["task"].as_str().unwrap_or(&front.id),
                "project": v.issue.project,
                "agent": r["agent"],
                // A question is open until answered; the record kinds
                // have no open state.
                "open": r.get("open").cloned().unwrap_or(Value::Null),
                "at": r["at"],
                // Same `created` alias as the intake rows.
                "created": r["at"],
                "session": r["session"],
                "sha": r["sha"],
                "state": r["state"],
                "answers": r["answers"],
                "answered_by": r.get("answered_by").cloned().unwrap_or(Value::Null),
                "verdict": r["verdict"],
                "pr": r["pr"],
                "summary": task_report::summary_line(
                    r["body"].as_str().unwrap_or_default(),
                    80,
                ),
            });
            if !r["error"].is_null() {
                row["error"] = r["error"].clone();
            }
            rows.push(row);
        }
    }
    let selecting = !f.kinds.is_empty()
        || !f.tickets.is_empty()
        || !f.agents.is_empty()
        || !f.projects.is_empty()
        || !f.sources.is_empty();
    rows.retain(|r| {
        crate::filter::any_of(&f.kinds, r["kind"].as_str())
            && crate::filter::any_of(&f.tickets, r["ticket"].as_str())
            && crate::filter::any_of(&f.agents, r["agent"].as_str())
            && crate::filter::any_of(&f.projects, r["project"].as_str())
            && crate::filter::any_of(&f.sources, r["source"].as_str())
    });
    rows.retain(|r| {
        if f.all {
            return true;
        }
        match r["open"].as_bool() {
            // open==true rows survive every scope.
            Some(true) => true,
            // Resolved rows need --all.
            Some(false) => false,
            // Records (`done`/`blocked`/`answer`/`verdict`) have no
            // open state: a filtered query keeps them, --open and the
            // bare triage view do not.
            None => selecting && !f.open,
        }
    });
    // Newest first by default; natural id breaks ties (X-16 after
    // X-9). `--sort` overrides the ordering.
    const REPORT_SORTS: &[(&str, &str)] = &[
        ("id", "id"),
        ("at", "at"),
        ("created", "created"),
        ("kind", "kind"),
        ("ticket", "ticket"),
        ("agent", "agent"),
        ("project", "project"),
        ("status", "status"),
        ("source", "source"),
    ];
    match &f.sort {
        Some(spec) => crate::filter::sort_rows(&mut rows, spec, REPORT_SORTS, "id")?,
        None => rows.sort_by(|a, b| {
            let aid = a["id"].as_str().unwrap_or_default();
            let bid = b["id"].as_str().unwrap_or_default();
            b["at"]
                .as_str()
                .cmp(&a["at"].as_str())
                .then_with(|| board::natural_key(bid).cmp(&board::natural_key(aid)))
        }),
    }
    crate::filter::apply_limit(&mut rows, f.limit);
    crate::filter::apply_fields(&mut rows, &f.fields)?;
    Ok(json!({"reports": rows, "count": rows.len()}))
}

/// `cadence report show <ID>` — one intake issue, body included.
/// Refuses non-intake issues: they belong to `issue` verbs.
pub fn show(pm: &Pm, id: &str) -> Result<Value> {
    let issue = board::find_issue(&pm.dir, id)?;
    if !issue.front.tags.iter().any(|t| t == "intake") {
        return Err(Error::rejected(format!(
            "{id} is not an intake issue — use `cadence issue` verbs for tracker issues"
        )));
    }
    Ok(json!({
        "id": issue.front.id, "project": issue.project,
        "kind": report_kind(&issue.front),
        "status": issue.front.status, "priority": issue.front.priority,
        "title": issue.front.title, "tags": issue.front.tags,
        "owner": issue.front.owner, "created": issue.front.created,
        "body": issue.body,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_args_redacts_token_and_flag_shapes() {
        let secret = format!("ghp_{}", "a".repeat(36));
        let out = scrub(&format!("leaked {secret} and --api-key={secret}"));
        assert!(!out.contains(&secret), "{out}");
        assert!(out.contains(REDACTED), "{out}");
        assert_eq!(scrub("ordinary words only"), "ordinary words only");
    }

    #[test]
    fn scrub_body_preserves_lines_and_indents() {
        let body = "Steps to reproduce:\n\n    1. run `cadence status`\n\t2. see error\n";
        let out = scrub_body(body);
        assert_eq!(out, body);
    }

    #[test]
    fn scrub_body_redacts_prose_secret_forms() {
        for (row, gone) in [
            (
                "Authorization: Basic dXNlcjpwYXNzd29yZA==",
                "dXNlcjpwYXNzd29yZA==",
            ),
            ("--password \"correct horse battery\"", "horse"),
            ("-u \"admin:hunter 2\"", "admin:hunter"),
            ("the db password is hunter2", "hunter2"),
            ("https://api/x?api_key=abcd1234", "abcd1234"),
            (
                "api_key = aGVsbG8td29ybGQtc2VjcmV0",
                "aGVsbG8td29ybGQtc2VjcmV0",
            ),
        ] {
            let out = scrub_body(row);
            assert!(!out.contains(gone), "{row:?} → {out:?}");
        }
    }

    #[test]
    fn scrub_body_masks_pem_and_keeps_prose() {
        let pem = "key material:\n-----BEGIN RSA PRIVATE KEY-----\nabc123\n-----END RSA PRIVATE KEY-----\ndone";
        let out = scrub_body(pem);
        assert!(!out.contains("abc123"), "{out}");
        assert!(!out.contains("PRIVATE KEY"), "{out}");
        // Ordinary prose is untouched — `key point`, `keyboard`,
        // `the token was invalid` carry no value to mask.
        assert_eq!(
            scrub_body("the key point is clear\nthe token was invalid"),
            "the key point is clear\nthe token was invalid"
        );
    }

    #[test]
    fn scrub_body_quoted_header_keeps_trailing_args() {
        let out =
            scrub_body("curl -H \"Authorization: Basic dXNlcjpwYXNz\" https://api.example.com");
        assert!(!out.contains("dXNlcjpwYXNz"), "{out}");
        assert!(out.contains("https://api.example.com"), "{out}");
        // A bare header line still masks to end-of-line.
        let bare = scrub_body("Authorization: Bearer abc.def.ghi tail");
        assert!(!bare.contains("abc.def.ghi"), "{bare}");
        assert!(!bare.contains("tail"), "{bare}");
    }

    #[test]
    fn scrub_body_collapses_quoted_span_to_one_marker() {
        let out = scrub_body("--password \"correct horse battery\"");
        assert!(!out.contains("horse"), "{out}");
        assert_eq!(out.matches(REDACTED).count(), 1, "{out}");
    }

    #[test]
    fn scrub_body_eats_across_line_break() {
        let out = scrub_body("password:\n  hunter2 rest");
        assert!(!out.contains("hunter2"), "{out}");
    }

    #[test]
    fn strip_controls_keeps_structure() {
        let out = strip_controls("a\x1b[2Jb\x07c\x00d\te\nf\rg");
        assert_eq!(out, "a[2Jbcd\te\nfg");
    }

    #[test]
    fn kind_defaults_and_routing_class() {
        assert_eq!(Kind::Bug.default_priority(), "P2");
        assert_eq!(Kind::Idea.default_priority(), "P3");
        assert!(Kind::Bug.about_cadence() && Kind::Question.about_cadence());
        assert!(!Kind::Idea.about_cadence());
    }
}
