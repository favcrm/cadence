//! `cadence session start` / `cadence session end` — the SESSION.md
//! bookends as two verbs.
//!
//! `start` is the morning gate: host, binary, daemon, board, reconcile
//! and inbox checks print `ok|warn|fail` with a one-line remedy and the
//! process exits 0 (all ok), 1 (warnings) or 2 (failures) — the PM's
//! first command of a day is the same every time. Read-only by
//! default; `--fix` performs only the reversible fixes (`daemon start`,
//! `ui start`, `ui tailscale start` when sharing is persisted). It
//! never restarts a running daemon and never removes anything.
//!
//! `end` is the evening sweep: the merged-worktree finish (CAD-93's
//! `issue finish --merged` as the library call, dry-run honoured),
//! `agent stop` for agents idle past `--idle-secs` with nothing queued
//! and no running message — re-verified against a live `agent_show`
//! immediately before each stop so a just-dispatched agent is skipped,
//! never killed mid-turn — `agent gc --older-than 1h`, a host sweep
//! (orphaned processes are reported, never killed, and process argv
//! never reaches output — orphans show executable + argument count),
//! note under `<state>/sessions/<timestamp>-end.md` (real runs never
//! overwrite; `--dry-run` writes nothing and previews the markdown).
//! It never stops a busy agent and never stops the daemon while work
//! is live.
//!
//! Every check is composed from the existing implementations — doctor,
//! `daemon_info`, `ui status`, overview's needs-me rows, the tracker
//! views, `issue finish`'s sweep — never re-computed here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::adapter::registry;
use crate::client;
use crate::doctor;
use crate::error::{Error, Result};
use crate::issue::{self, board, project, time as itime};
use crate::overview;
use crate::ui;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sev {
    Ok,
    Warn,
    Fail,
}

impl Sev {
    fn name(self) -> &'static str {
        match self {
            Sev::Ok => "ok",
            Sev::Warn => "warn",
            Sev::Fail => "fail",
        }
    }
}

/// One reported line: the check, its worst finding, and the fix.
struct Row {
    name: &'static str,
    sev: Sev,
    detail: String,
    remedy: Option<String>,
    /// What `--fix` did, when it acted.
    fixed: Option<String>,
    /// Sub-findings inside one named check (reconcile rows).
    items: Vec<String>,
    /// Ack key of the finding the detail line names (the host row's
    /// head check) and its acknowledgement note.
    key: Option<String>,
    ack: Option<String>,
    /// Keyed sub-findings — each one acknowledgeable on its own.
    findings: Vec<Finding>,
}

/// One keyed finding inside a check — the unit an operator
/// acknowledges (`cadence session ack <key>`) and the unit the cwd
/// scope sorts (CAD-257). `project` is `None` when nothing attributes
/// it to a project: an unattributed finding always stays in scope, so
/// the scope can only narrow what it can name (fail closed).
struct Finding {
    key: String,
    project: Option<String>,
    sev: Sev,
    text: String,
    /// `(acknowledged until …: reason)` or `(acknowledgement expired …)`.
    ack: Option<String>,
}

impl Finding {
    fn new(key: String, project: Option<String>, sev: Sev, text: String) -> Self {
        Self {
            key,
            project,
            sev,
            text,
            ack: None,
        }
    }

    fn json(&self) -> Value {
        json!({
            "key": self.key, "project": self.project,
            "severity": self.sev.name(), "text": scrub_line(&self.text),
            "acknowledged": self.ack.as_deref().map(scrub_line),
        })
    }
}

impl Row {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            sev: Sev::Ok,
            detail: String::new(),
            remedy: None,
            fixed: None,
            items: Vec::new(),
            key: None,
            ack: None,
            findings: Vec::new(),
        }
    }

    fn ok(mut self, detail: impl Into<String>) -> Self {
        self.sev = Sev::Ok;
        self.detail = detail.into();
        self
    }

    fn warn(mut self, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        self.sev = Sev::Warn;
        self.detail = detail.into();
        let r = remedy.into();
        if !r.is_empty() {
            self.remedy = Some(r);
        }
        self
    }

    fn fail(mut self, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        self.sev = Sev::Fail;
        self.detail = detail.into();
        let r = remedy.into();
        if !r.is_empty() {
            self.remedy = Some(r);
        }
        self
    }

    fn json(&self) -> Value {
        json!({
            "name": self.name, "severity": self.sev.name(),
            "detail": scrub_line(&self.detail),
            "remedy": self.remedy.as_deref().map(scrub_line),
            "fixed": self.fixed.as_deref().map(scrub_line),
            "items": self.items.iter().map(|i| scrub_line(i)).collect::<Vec<_>>(),
            "key": self.key,
            "acknowledged": self.ack.as_deref().map(scrub_line),
            "findings": self.findings.iter().map(Finding::json).collect::<Vec<_>>(),
        })
    }
}

/// `key<sep>value` credential spans that argv shape can't see — text
/// like `Authorization: Basic <cred>` or `--auth-token "Bearer <tok>"`,
/// where the value may be a two-token `scheme credential` pair or sit
/// inside quotes. A keyword counts only bounded by non-alphanumerics
/// on both sides (`Authorization:` and `x-api-key` qualify; `monkey`,
/// `author`, `keystore` don't — the unbounded scan ate the word after
/// them) — except glued env names, which need an explicit `=`/`:` to
/// count (`PGPASSWORD=hunter2` masks, `monkey business` stays prose).
/// `=`/`:` and quoted values mask unconditionally; a whitespace-only
/// separator masks only when the value `looks_secret` — `token is
/// expired` and `pytest tests/auth test_x` are not credentials.
fn scrub_auth_spans(s: &str) -> String {
    const KEYWORDS: &[&str] = &[
        "authorization",
        "credentials",
        "credential",
        "password",
        "passwd",
        "passphrase",
        "apikey",
        "bearer",
        "secret",
        "token",
        "access",
        "cookie",
        "private",
        "auth",
        "cred",
        "pass",
        "pwd",
        "key",
    ];
    let b = s.as_bytes();
    let alnum = |c: u8| c.is_ascii_alphanumeric();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let kw = KEYWORDS.iter().find_map(|kw| {
            (i + kw.len() <= b.len()
                && b[i..i + kw.len()].eq_ignore_ascii_case(kw.as_bytes())
                && (i == 0
                    || !alnum(b[i - 1])
                    || (i + kw.len() < b.len() && matches!(b[i + kw.len()], b'=' | b':')))
                && (i + kw.len() == b.len() || !alnum(b[i + kw.len()])))
            .then_some(kw.len())
        });
        let Some(kw) = kw else {
            let n = s[i..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&s[i..i + n]);
            i += n;
            continue;
        };
        // A keyword counts only with a separator and a value after it.
        let mut j = i + kw;
        while j < b.len() && matches!(b[j], b'=' | b':' | b' ' | b'\t') {
            j += 1;
        }
        if j == i + kw || j >= b.len() {
            out.push_str(&s[i..j]);
            i = j;
            continue;
        }
        // `=`/`:` in the run means the value is implied — mask
        // unconditionally. Whitespace-only means prose may follow;
        // the value must look secret to mask.
        let implied = s[i + kw..j].contains(['=', ':']);
        if b[j] == b'"' || b[j] == b'\'' {
            let q = b[j];
            out.push_str(&s[i..=j]);
            let mut k = j + 1;
            while k < b.len() && b[k] != q {
                k += 1;
            }
            out.push_str("[REDACTED]");
            if k < b.len() {
                out.push(q as char);
                k += 1;
            }
            i = k;
            continue;
        }
        let mut k = j;
        while k < b.len() && !b[k].is_ascii_whitespace() {
            k += 1;
        }
        // An alpha-only first token is a scheme word (`Basic`,
        // `Bearer`, `Token`, `ApiKey`, `Negotiate`) — the credential
        // itself is the token after it. Space-separated it must
        // still look secret to mask.
        if s[j..k].bytes().all(|c| c.is_ascii_alphabetic()) {
            let mut m0 = k;
            while m0 < b.len() && b[m0].is_ascii_whitespace() {
                m0 += 1;
            }
            let mut m = m0;
            while m < b.len() && !b[m].is_ascii_whitespace() {
                m += 1;
            }
            if implied || looks_secret(&s[m0..m]) {
                out.push_str(&s[i..j]);
                out.push_str("[REDACTED]");
                i = m;
                continue;
            }
        } else if implied || looks_secret(&s[j..k]) {
            // Keep a quote the value ran up against — `-H
            // "Authorization: Basic x=="` eats through the close
            // quote otherwise.
            let quote = k > j && matches!(b[k - 1], b'"' | b'\'');
            if quote {
                k -= 1;
            }
            out.push_str(&s[i..j]);
            out.push_str("[REDACTED]");
            i = k;
            continue;
        }
        // Not a credential use — emit through the separators and let
        // the value text scan on its own.
        out.push_str(&s[i..j]);
        i = j;
    }
    out
}

/// Whitespace-separator gate for `scrub_auth_spans` — is this token
/// secret-looking enough to mask on a bare space? `hunter2` yes
/// (length plus a digit), `expired`/`test_x`/`localhost` no.
fn looks_secret(tok: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "figd_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "xoxa-",
        "xoxb-",
        "xoxp-",
        "xoxr-",
        "xoxs-",
        "AKIA",
        "ASIA",
    ];
    let t = tok.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')'));
    t.len() >= 12
        || (t.len() >= 6 && t.bytes().any(|b| b.is_ascii_digit()))
        || t.ends_with('=')
        || PREFIXES.iter().any(|p| t.starts_with(p))
        || (t.starts_with("eyJ") && t.contains('.'))
}

/// `docker login -u <user> <password>` — the password is positional:
/// no keyword names it and a plain value carries no credential
/// shape. Only the token after the `-u`/`--user` value masks, and
/// only for `login`.
fn mask_docker_login_positional(words: &mut [String]) {
    let Some(login) = words.iter().position(|w| w == "login") else {
        return;
    };
    let Some(u) = words.iter().position(|w| w == "-u" || w == "--user") else {
        return;
    };
    if u <= login {
        return;
    }
    if let Some(w) = words.get_mut(u + 2).filter(|w| !w.starts_with('-')) {
        *w = "[REDACTED]".to_string();
    }
}

/// An unquoted token run through docker-positional masking and the
/// shared argv scrubber — `(rendered, masked?)`.
fn scrub_tokens(words: &[String]) -> (String, bool) {
    let mut w = words.to_vec();
    let pre = w.join(" ");
    mask_docker_login_positional(&mut w);
    let red = doctor::host::redact_argv(&w);
    let masked = red != pre;
    (red, masked)
}

/// One display line: `scrub_auth_spans` masks `key<sep>value`
/// expressions argv-tokenization can't see, then CAD-108's shared
/// `doctor::host::redact_argv` masks flag/env/URI/credential-shape
/// values per token; `mask_docker_login_positional` covers `login
/// -u`'s bare positional password. A quoted run is one argv element's
/// surface — whitespace inside it must not split it into independent
/// tokens, or a secret *inside* a single element still prints
/// (CAD-141): the interior scrubs as a unit and the whole blob masks
/// when it differs. Process argv itself never reaches here — orphans
/// display as `exe (arg count)`. A line nothing masked keeps its
/// original whitespace.
fn scrub_line(s: &str) -> String {
    let spanned = scrub_auth_spans(s);
    let b = spanned.as_bytes();
    let mut changed = false;
    let mut out = String::with_capacity(spanned.len());
    let mut words: Vec<String> = Vec::new();
    let mut pending_ws = false;
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            pending_ws = !out.is_empty() || !words.is_empty();
            i += 1;
            continue;
        }
        // A quote with a matching close opens a blob — one argv
        // element's worth of text, interior whitespace included.
        if matches!(b[i], b'"' | b'\'') && spanned[i + 1..].contains(b[i] as char) {
            if !words.is_empty() {
                let (red, ch) = scrub_tokens(&words);
                if pending_ws && !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&red);
                changed |= ch;
                words.clear();
            }
            let q = b[i];
            let mut j = i + 1;
            while j < b.len() && b[j] != q {
                j += 1;
            }
            let iw: Vec<String> = spanned[i + 1..j]
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let (_red, ch) = scrub_tokens(&iw);
            if pending_ws && !out.is_empty() {
                out.push(' ');
            }
            pending_ws = false;
            if ch && !iw.is_empty() {
                // The element's interior masks — emit the whole blob
                // masked, keeping its quotes.
                changed = true;
                out.push(q as char);
                out.push_str("[REDACTED]");
                if j < b.len() {
                    out.push(q as char);
                }
            } else {
                let end = if j < b.len() { j + 1 } else { j };
                out.push_str(&spanned[i..end]);
            }
            i = if j < b.len() { j + 1 } else { j };
            continue;
        }
        // A plain word — ends at whitespace or at a quote that opens
        // a blob (an unmatched quote is literal text, not a blob).
        let mut j = i;
        while j < b.len()
            && !b[j].is_ascii_whitespace()
            && !(matches!(b[j], b'"' | b'\'') && spanned[j + 1..].contains(b[j] as char))
        {
            j += 1;
        }
        words.push(spanned[i..j].to_string());
        i = j;
    }
    if !words.is_empty() {
        let (red, ch) = scrub_tokens(&words);
        if pending_ws && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&red);
        changed |= ch;
    }
    if !changed {
        return spanned;
    }
    // `auth:` is itself a header shape — the argv pass re-masks it
    // and keeps the span's own mask token, doubling the marker.
    let mut scrubbed = out;
    while scrubbed.contains("[REDACTED] [REDACTED]") {
        scrubbed = scrubbed.replace("[REDACTED] [REDACTED]", "[REDACTED]");
    }
    scrubbed
}

/// The host scan with the command's cwd — `Scan::host` defaults to
/// the process cwd, which is the same thing in practice but the
/// explicit override keeps the option field honest.
/// `fixture <path>` when the host report came from `--host-report`,
/// `scan` when the real `doctor::host::run` ran — the `--json` payload
/// labels it so no fixture run reads as a real scan.
fn host_source(fixture: Option<&Path>) -> String {
    fixture
        .map(|p| format!("fixture {}", p.display()))
        .unwrap_or_else(|| "scan".to_string())
}

fn host_scan_for(state_dir: PathBuf, cwd: PathBuf) -> doctor::host::Scan {
    let mut scan = doctor::host::Scan::host(&state_dir);
    scan.cwd = cwd;
    scan
}

/// The host report — `doctor::host::run`, or the JSON fixture passed
/// explicitly via `--host-report`. Not an env var: an ambient
/// `CADENCE_*` must never soften the go/no-go gate, and an unreadable
/// or unparsable fixture is a hard error — never a silent fallthrough
/// to the real scan (a typo'd path would read the real host again with
/// no signal, reintroducing the coupling the fixture exists to remove).
fn host_report(scan: &doctor::host::Scan, fixture: Option<&Path>) -> Result<Value> {
    match fixture {
        None => Ok(doctor::host::run(scan)),
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| Error::rejected(format!("--host-report {}: {e}", path.display())))?;
            serde_json::from_str(&text).map_err(|e| {
                Error::rejected(format!(
                    "--host-report {}: not a doctor --host report ({e})",
                    path.display()
                ))
            })
        }
    }
}

/// Display detail for one check — the orphans check's own detail
/// embeds cmdlines, and session output never prints argv (CAD-108 QA:
/// every scrubber leaks some shape — `user:tok@host`, joined
/// `-p<pass>`), so it degrades to a count. Checks without `pids`
/// (fixture shorthand) keep their detail — it's the fixture's text.
fn check_detail(c: &Value) -> String {
    if c["name"].as_str() == Some("orphans") {
        if let Some(pids) = c["value"]["pids"].as_array() {
            return format!("{} orphaned pid(s)", pids.len());
        }
    }
    c["detail"].as_str().unwrap_or_default().to_string()
}

/// argv0 → the executable name. argv0 can be one shell string holding
/// the whole command — `sh -c 'npm exec --api-key=…'` — so the
/// executable is the first word, then its basename.
fn exe_of(argv0: &str) -> String {
    let word = argv0.split_whitespace().next().unwrap_or_default();
    Path::new(word)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// `/proc/<pid>/cmdline` re-read at display time — the orphan line is
/// the executable basename and the argument count, never the
/// arguments. A pid that vanished or is unreadable still names the
/// pid; `head` (the raw cmdline) is never a fallback.
fn orphan_pid_item(o: &Value) -> String {
    let pid = o["pid"].as_u64().unwrap_or_default();
    let reasons = o["reasons"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let exe_argc = std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|raw| {
            let parts: Vec<&[u8]> = raw.split(|b| *b == 0).filter(|s| !s.is_empty()).collect();
            parts.first().map(|p| {
                format!(
                    "{} ({} arg(s))",
                    exe_of(&String::from_utf8_lossy(p)),
                    parts.len()
                )
            })
        })
        .unwrap_or_else(|| "(argv unavailable)".to_string());
    format!("orphan pid {pid} — {exe_argc} ({reasons})")
}

/// Per-pid `exe (N arg(s))` lines for an orphans check — the argv-free
/// replacement for cmdline heads. Capped: the count detail already
/// carries the full number.
fn orphan_items(c: &Value) -> Vec<String> {
    let Some(pids) = c["value"]["pids"].as_array() else {
        return vec![];
    };
    let mut items: Vec<String> = pids.iter().take(8).map(orphan_pid_item).collect();
    if pids.len() > 8 {
        items.push(format!("… and {} more orphan pid(s)", pids.len() - 8));
    }
    items
}

/// `ok|warn|fail` text → `Sev`; anything else reads as ok.
fn sev_of(level: &Value) -> Sev {
    match level.as_str() {
        Some("fail") => Sev::Fail,
        Some("warn") => Sev::Warn,
        _ => Sev::Ok,
    }
}

/// A `doctor --host` report as one Row: worst level wins, each non-ok
/// check becomes a keyed finding (`host:<check>`) with its remedy — so
/// a known host condition can be acknowledged like any other item.
/// Orphan checks get count-only details plus per-pid `exe (arg count)`
/// items — no argv. A fixture (`--host-report`) is labelled in the row
/// — no fixture run can be mistaken for a real scan.
fn host_row(
    name: &'static str,
    report: &Value,
    fixture: Option<&Path>,
    acks: &Acks,
    now: i64,
) -> Row {
    let mut row = Row::new(name);
    if let Some(path) = fixture {
        row.items.push(format!(
            "fixture {} — real host not scanned",
            path.display()
        ));
    }
    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let bad: Vec<&Value> = checks
        .iter()
        .filter(|c| c["level"].as_str().unwrap_or("ok") != "ok")
        .collect();
    let head = bad
        .iter()
        .position(|c| c["level"].as_str() == Some("fail"))
        .unwrap_or(0);
    // A report with no non-ok checks keeps its own level (fixture
    // shorthand); otherwise the worst check after acknowledgements.
    let mut worst = if bad.is_empty() {
        sev_of(&report["level"])
    } else {
        Sev::Ok
    };
    for (i, c) in bad.iter().enumerate() {
        let cname = c["name"].as_str().unwrap_or("?");
        let detail = check_detail(c);
        let remedy = c["remedy"].as_str().unwrap_or_default();
        let text = if i == head || remedy.is_empty() {
            format!("{cname}: {detail}")
        } else {
            format!("{cname}: {detail} — {remedy}")
        };
        let mut f = Finding::new(format!("host:{cname}"), None, sev_of(&c["level"]), text);
        acks.apply(&mut f, now);
        worst = worst.max(f.sev);
        if i == head {
            row.detail = f.text;
            if !remedy.is_empty() {
                row.remedy = Some(remedy.to_string());
            }
            row.key = Some(f.key);
            row.ack = f.ack;
        } else {
            row.findings.push(f);
        }
        row.items.extend(orphan_items(c));
    }
    if bad.is_empty() {
        row.detail = "host clean".to_string();
    }
    row.sev = worst;
    row
}

/// Print `first` after `lead`, then any further lines of a multi-line
/// text (a kill remedy is one line per pid) indented under it. Every
/// line is scrubbed on its own — a newline is not an argv separator.
fn print_lines(lead: &str, text: &str, tail: &str) {
    let mut lines = text.lines();
    println!(
        "{lead}{}{tail}",
        scrub_line(lines.next().unwrap_or_default())
    );
    for l in lines {
        println!("               {}", scrub_line(l));
    }
}

/// ` (acknowledged until …: reason)` — empty without an ack note.
fn ack_tail(ack: &Option<String>) -> String {
    ack.as_deref()
        .map(|a| format!(" {}", scrub_line(a)))
        .unwrap_or_default()
}

fn print_row(r: &Row) {
    // Keys print unscrubbed: they are ids, aliases and paths the
    // gate built itself, and an operator must copy them verbatim.
    let key = r
        .key
        .as_deref()
        .map(|k| format!("[{k}] "))
        .unwrap_or_default();
    let mut line = format!(
        "{:<9} {:<4} {key}{}",
        r.name,
        r.sev.name(),
        scrub_line(&r.detail)
    );
    // A multi-line remedy (named kill lines) joins the detail line with
    // its first line; the rest follow indented.
    let mut rest = "".lines();
    if let Some(rem) = &r.remedy {
        let mut lines = rem.lines();
        line.push_str(&format!(
            " — {}",
            scrub_line(lines.next().unwrap_or_default())
        ));
        rest = lines;
    }
    println!("{line}{}", ack_tail(&r.ack));
    for l in rest {
        println!("               {}", scrub_line(l));
    }
    for i in &r.items {
        print_lines("           · ", i, "");
    }
    for f in &r.findings {
        print_lines(
            &format!("           · [{}] ", f.key),
            &f.text,
            &ack_tail(&f.ack),
        );
    }
    if let Some(f) = &r.fixed {
        println!("           fixed: {}", scrub_line(f));
    }
}

// ---------- shared probes ----------

/// Agent rows plus each one's `agent_show` — the snapshot both verbs
/// read. Empty when the daemon is unreachable.
struct Fleet {
    reachable: bool,
    info: Option<Value>,
    agents: Vec<Value>,
    shows: HashMap<String, Value>,
}

fn fleet(state_dir: &Path) -> Fleet {
    let reachable = client::rpc(state_dir, "health", json!({})).is_ok();
    let info = client::rpc(state_dir, "daemon_info", json!({})).ok();
    let agents = client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .unwrap_or_default();
    let mut shows = HashMap::new();
    for a in &agents {
        if let Some(alias) = a["alias"].as_str() {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
                shows.insert(alias.to_string(), show);
            }
        }
    }
    Fleet {
        reachable,
        info,
        agents,
        shows,
    }
}

/// The in-flight message for one agent — `running` or `submitting` —
/// as `(id, first-line, age_secs)`.
fn running_msg(show: &Value, now: i64) -> Option<(String, String, i64)> {
    show["messages"].as_array()?.iter().find_map(|m| {
        if !matches!(
            m["state"].as_str().unwrap_or_default(),
            "running" | "submitting"
        ) {
            return None;
        }
        let started = m["started"]
            .as_f64()
            .or(m["created"].as_f64())
            .unwrap_or(now as f64) as i64;
        let head = m["body"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect::<String>();
        Some((
            m["id"].as_str().unwrap_or_default().to_string(),
            head,
            (now - started).max(0),
        ))
    })
}

/// Whether an agent is safe to stop right now — the one predicate the
/// candidate pass (against the fleet snapshot) and the pre-stop
/// re-check (against a fresh `agent_show` taken immediately before
/// every `agent_stop`) share, so they can never drift again: an agent
/// that claimed work while the sweep ran is skipped, never killed
/// mid-turn. `Busy` means live work showed (a running message, or a
/// pane that wouldn't probe idle); `No` means it simply wasn't a
/// candidate. RPC/probe failures fail closed as `No`/`Busy`, never
/// `Yes`.
enum Stop {
    Yes,
    Busy,
    No,
}

fn stoppable(state_dir: &Path, agent: &Value, show: &Value, idle_secs: u64, now: i64) -> Stop {
    if running_msg(show, now).is_some() {
        return Stop::Busy;
    }
    if agent["state"].as_str() != Some("idle")
        || agent["dead"].as_bool().unwrap_or(false)
        || show["queued"].as_i64().unwrap_or(0) > 0
    {
        return Stop::No;
    }
    let updated = agent["updated"].as_f64().unwrap_or(now as f64) as i64;
    if now - updated < idle_secs as i64 {
        return Stop::No;
    }
    // Never stop a busy pane: a live pty endpoint must probe idle.
    if agent["endpoint_kind"].as_str().unwrap_or_default() == "pty" && agent["endpoint"].is_string()
    {
        let idle = client::rpc(
            state_dir,
            "agent_probe",
            json!({"alias": agent["alias"].as_str().unwrap_or_default()}),
        )
        .map(|p| p["idle"].as_bool().unwrap_or(false))
        .unwrap_or(false);
        return if idle { Stop::Yes } else { Stop::Busy };
    }
    Stop::Yes
}

/// Tracker projects and views for `scope` (`None` = all projects), plus
/// each project's locally-declared repo checkouts for the fs scans.
struct Scope {
    projects: Vec<project::Project>,
    views: Vec<board::View>,
    /// `(project key, github slug if the remote is github, local path)`.
    repos: Vec<(String, Option<String>, PathBuf)>,
    /// Issue id (uppercased) → status, for the worktree-name match.
    issue_status: HashMap<String, String>,
}

fn scope(project: Option<&str>) -> Result<Scope> {
    let mut out = Scope {
        projects: Vec::new(),
        views: Vec::new(),
        repos: Vec::new(),
        issue_status: HashMap::new(),
    };
    let Ok(pm) = issue::Pm::open_default() else {
        return Ok(out);
    };
    let all = project::list(&pm.dir)?;
    let mut projects = all.clone();
    if let Some(want) = project {
        if !all.iter().any(|p| p.key == want) {
            return Err(Error::rejected(format!(
                "unknown project '{want}' — `cadence issue project ls` lists the keys"
            )));
        }
        projects.retain(|p| p.key == want);
    }
    for p in &projects {
        for r in &p.repos {
            let Some(path) = r.path.as_deref().map(project::expand_home) else {
                continue;
            };
            let slug = r
                .remote
                .as_deref()
                .map(project::normalize_remote)
                .and_then(|n| n.strip_prefix("github.com/").map(str::to_string));
            out.repos.push((p.key.clone(), slug, path));
        }
    }
    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
    let views = board::views(&pm.config.notes_dir(), issues);
    for v in views {
        if project.is_some() && v.issue.project != project.unwrap_or_default() {
            continue;
        }
        out.issue_status
            .insert(v.issue.front.id.to_uppercase(), v.status.clone());
        out.views.push(v);
    }
    // All projects — `build_repo_match` must find the build's repo
    // even when `--project` narrowed the checks above.
    out.projects = all;
    Ok(out)
}

/// The `cadence/<wt-name>` open-PR branch names, lowercased, plus the
/// full per-slug `prs` payload for the handoff — one `gh` fetch shared
/// with the overview cache.
fn gh_open(
    state_dir: &Path,
    sc: &Scope,
    cache_only: bool,
) -> (Vec<String>, HashMap<String, Value>) {
    let mut slugs: Vec<String> = sc.repos.iter().filter_map(|(_, s, _)| s.clone()).collect();
    slugs.sort();
    slugs.dedup();
    // A dry run writes nothing — the gh cache included — so it reads
    // whatever a previous real fetch left instead of refreshing.
    let (repos, _state) = if cache_only {
        overview::github_repos_cached(state_dir, &slugs)
    } else {
        overview::github_repos(state_dir, &slugs)
    };
    let mut branches = Vec::new();
    for data in repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(h) = pr["headRefName"].as_str() {
                branches.push(h.to_lowercase());
            }
        }
    }
    (branches, repos)
}

/// `.cadence/wt/*` dirs under each repo checkout, skipping tool-owned
/// review trees (the `.cadence-review-tree` marker `cadence review`
/// leaves) and names without a `<prefix>-<n>-` issue stem.
fn worktree_dirs(repos: &[(String, Option<String>, PathBuf)]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for (_, _, root) in repos {
        let wt = root.join(".cadence").join("wt");
        let Ok(entries) = std::fs::read_dir(&wt) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            if e.path().join(".cadence-review-tree").is_file() {
                continue;
            }
            if let Some(name) = e.file_name().to_str() {
                out.push((root.clone(), name.to_string()));
            }
        }
    }
    out
}

/// `<prefix>-<num>` issue id out of a `cad-70-review-verb` worktree
/// name — `cadence/<name>` branches and `.cadence/wt/<name>` dirs share
/// the stem.
fn issue_stem(name: &str) -> Option<String> {
    let mut parts = name.splitn(3, '-');
    let prefix = parts.next()?;
    let num: u64 = parts.next()?.parse().ok()?;
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some(format!("{}-{}", prefix.to_uppercase(), num))
}

// ---------- acknowledgements ----------

/// The longest an acknowledgement may run — a known backlog is parked
/// for two weeks at most, then it fails the gate again.
const ACK_MAX_SECS: i64 = 14 * 86_400;

/// One `cadence session ack` record. Times are ISO-8601 UTC.
#[derive(Clone, Serialize, Deserialize)]
struct Ack {
    key: String,
    reason: String,
    actor: String,
    created: String,
    expires: String,
}

#[derive(Default, Serialize, Deserialize)]
struct AckFile {
    acks: Vec<Ack>,
}

/// The acknowledgements the gate reads. `error` names an unreadable
/// store — the gate then applies none (an ack can only soften) and
/// says so in its own row.
#[derive(Default)]
struct Acks {
    list: Vec<Ack>,
    error: Option<String>,
}

impl Acks {
    fn load(state_dir: &Path) -> Self {
        match read_acks(state_dir) {
            Ok(list) => Self { list, error: None },
            Err(e) => Self {
                list: Vec::new(),
                error: Some(e.to_string()),
            },
        }
    }

    /// The record with the latest expiry for `key` decides: unexpired,
    /// a `fail` finding downgrades to `warn` and prints `(acknowledged
    /// until …: reason)`; expired, the finding keeps its severity and
    /// prints `(acknowledgement expired …)`. An ack never hides an item.
    fn apply(&self, f: &mut Finding, now: i64) {
        let Some(a) = self
            .list
            .iter()
            .filter(|a| a.key == f.key)
            .max_by_key(|a| itime::parse_iso(&a.expires).unwrap_or(0))
        else {
            return;
        };
        if itime::parse_iso(&a.expires).is_some_and(|e| e > now) {
            if f.sev == Sev::Fail {
                f.sev = Sev::Warn;
            }
            f.ack = Some(format!("(acknowledged until {}: {})", a.expires, a.reason));
        } else {
            f.ack = Some(format!(
                "(acknowledgement expired {}: {})",
                a.expires, a.reason
            ));
        }
    }
}

/// `<state>/sessions/acks.json` — beside the handoff notes.
fn acks_path(state_dir: &Path) -> PathBuf {
    state_dir.join("sessions").join("acks.json")
}

/// Every recorded acknowledgement, expired ones included. A missing
/// file is an empty store; an unparsable one is an error, never an
/// empty store (a rewrite would drop the history).
fn read_acks(state_dir: &Path) -> Result<Vec<Ack>> {
    let path = acks_path(state_dir);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::internal(format!("{}: {e}", path.display()))),
    };
    serde_json::from_str::<AckFile>(&text)
        .map(|f| f.acks)
        .map_err(|e| Error::internal(format!("{} is not an ack store: {e}", path.display())))
}

/// `--expires`: a duration (`90m`, `12h`, `3d`, bare seconds) from
/// `now`, or an absolute `YYYY-MM-DDTHH:MM:SSZ`. Must land in the
/// future and within `ACK_MAX_SECS`.
fn parse_expiry(spec: &str, now: i64) -> Result<i64> {
    let at = match itime::parse_iso(spec) {
        Some(t) => t,
        None => {
            let (num, mult) = match spec.chars().last() {
                Some('s') => (&spec[..spec.len() - 1], 1),
                Some('m') => (&spec[..spec.len() - 1], 60),
                Some('h') => (&spec[..spec.len() - 1], 3_600),
                Some('d') => (&spec[..spec.len() - 1], 86_400),
                _ => (spec, 1),
            };
            let n: i64 = num.parse().map_err(|_| {
                Error::rejected(format!(
                    "--expires '{spec}': use a duration (90m, 12h, 3d) or YYYY-MM-DDTHH:MM:SSZ"
                ))
            })?;
            now.saturating_add(n.saturating_mul(mult))
        }
    };
    if at <= now {
        return Err(Error::rejected(format!(
            "--expires '{spec}' is not in the future"
        )));
    }
    if at - now > ACK_MAX_SECS {
        return Err(Error::rejected(format!(
            "--expires '{spec}' is past the 14-day maximum — acknowledge again when it lapses"
        )));
    }
    Ok(at)
}

/// Who acknowledged: the cadence alias inside a pane, else the OS user.
fn actor() -> String {
    std::env::var("CADENCE_ALIAS")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

pub struct AckOptions {
    pub key: Option<String>,
    pub reason: Option<String>,
    pub expires: Option<String>,
    pub list: bool,
    pub json: bool,
    pub state_dir: PathBuf,
}

/// `cadence session ack <key> --reason … --expires …` records one
/// acknowledgement; `--list` prints every record, expired ones marked
/// expired. Records are appended, never rewritten or pruned.
pub fn run_ack(opts: &AckOptions) -> Result<i32> {
    let now = itime::now_epoch();
    let path = acks_path(&opts.state_dir);
    let mut acks = read_acks(&opts.state_dir)?;
    if opts.list {
        let state = |a: &Ack| {
            if itime::parse_iso(&a.expires).is_some_and(|e| e > now) {
                "active"
            } else {
                "expired"
            }
        };
        if opts.json {
            let rows: Vec<Value> = acks
                .iter()
                .map(|a| {
                    let mut v = serde_json::to_value(a).unwrap_or_default();
                    v["state"] = json!(state(a));
                    v
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "kind": "session-acks", "path": path, "acks": rows,
                }))
                .unwrap_or_default()
            );
        } else if acks.is_empty() {
            println!("no acknowledgements ({})", path.display());
        } else {
            println!("acknowledgements ({})", path.display());
            for a in &acks {
                let st = state(a);
                println!(
                    "{st:<8} {}  {} {}  by {} at {}: {}",
                    a.key,
                    if st == "active" { "until" } else { "expired" },
                    a.expires,
                    a.actor,
                    a.created,
                    scrub_line(&a.reason)
                );
            }
        }
        return Ok(0);
    }
    let key = opts
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty() && !k.contains(char::is_whitespace))
        .ok_or_else(|| {
            Error::rejected("an ack needs the item key session start printed in [brackets]")
        })?;
    let reason = opts
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| Error::rejected("--reason is required — say why this is known"))?;
    // CAD-109: the reason is agent-authored text written durably — a
    // credential-shaped reason is refused before anything is written.
    let allow = crate::secret::Allowlist::load(&opts.state_dir)?;
    let secret_warnings = crate::secret::guard_with("session ack", reason, &allow)?;
    let expires = parse_expiry(
        opts.expires
            .as_deref()
            .ok_or_else(|| Error::rejected("--expires is required (at most 14d)"))?,
        now,
    )?;
    let ack = Ack {
        key: key.to_string(),
        reason: reason.to_string(),
        actor: actor(),
        created: itime::iso(now),
        expires: itime::iso(expires),
    };
    acks.push(ack.clone());
    let dir = opts.state_dir.join("sessions");
    let tmp = dir.join(format!(".acks.json.{}", std::process::id()));
    let body = serde_json::to_string_pretty(&AckFile { acks })
        .map_err(|e| Error::internal(e.to_string()))?;
    std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&tmp, body))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "session-ack", "path": path, "ack": ack,
                "secret_warnings": crate::secret::warnings_json(&secret_warnings),
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "acknowledged {} until {} — {} ({})",
            ack.key,
            ack.expires,
            scrub_line(&ack.reason),
            path.display()
        );
    }
    Ok(0)
}

// ---------- scope ----------

/// Which project `session start` judges. Findings outside it collapse
/// into one summary row that never fails the gate.
enum Focus {
    /// Fleet-wide — `--all`, or (with the note) no project matched the
    /// cwd.
    All(Option<String>),
    /// `--project`, or the project whose repo holds the cwd.
    Project(String),
}

impl Focus {
    fn covers(&self, project: Option<&str>) -> bool {
        match (self, project) {
            (Focus::All(_), _) | (_, None) => true,
            (Focus::Project(want), Some(p)) => want == p,
        }
    }

    fn json(&self) -> Value {
        match self {
            Focus::All(note) => json!({"project": null, "all": true, "note": note}),
            Focus::Project(p) => json!({"project": p, "all": false}),
        }
    }
}

/// The project whose declared repo holds `cwd` — `issue`'s own match
/// (remote first, then checkout path) without `CADENCE_PROJECT`: an
/// ambient env var must never narrow what the gate judges.
fn cwd_project(projects: &[project::Project], cwd: &Path) -> Option<String> {
    let (root, remote) = project::repo_identity(cwd)?;
    if let Some(remote) = &remote {
        let hit = projects.iter().find(|p| {
            p.repos.iter().any(|r| {
                r.remote
                    .as_deref()
                    .is_some_and(|x| project::normalize_remote(x) == *remote)
            })
        });
        if let Some(p) = hit {
            return Some(p.key.clone());
        }
    }
    projects
        .iter()
        .find(|p| {
            p.repos.iter().any(|r| {
                r.path.as_deref().is_some_and(|path| {
                    let path = project::expand_home(path);
                    path.canonicalize().unwrap_or(path) == root
                })
            })
        })
        .map(|p| p.key.clone())
}

/// Alias → project for every agent something attributes: its cwd under
/// a declared repo checkout (deepest match), else the one project whose
/// open issues it owns. Anything else stays unattributed.
fn agent_projects(agents: &[Value], sc: &Scope) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for a in agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        let cwd = Path::new(a["cwd"].as_str().unwrap_or_default());
        let by_cwd = sc
            .repos
            .iter()
            .filter(|(_, _, p)| {
                // An empty declared path starts_with()s every cwd.
                !p.as_os_str().is_empty() && !cwd.as_os_str().is_empty() && cwd.starts_with(p)
            })
            .max_by_key(|(_, _, p)| p.components().count())
            .map(|(k, _, _)| k.clone());
        let key = by_cwd.or_else(|| {
            let owned: HashSet<&str> = sc
                .views
                .iter()
                .filter(|v| {
                    v.issue.front.owner.as_deref() == Some(alias)
                        && !matches!(v.status.as_str(), "done" | "dropped")
                })
                .map(|v| v.issue.project.as_str())
                .collect();
            (owned.len() == 1).then(|| owned.into_iter().next().unwrap_or_default().to_string())
        });
        if let Some(k) = key {
            out.insert(alias.to_string(), k);
        }
    }
    out
}

/// `CAD-12` — an uppercase prefix, a dash, digits.
fn is_issue_id(w: &str) -> bool {
    w.split_once('-').is_some_and(|(p, n)| {
        !p.is_empty()
            && p.starts_with(|c: char| c.is_ascii_uppercase())
            && p.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && !n.is_empty()
            && n.chars().all(|c| c.is_ascii_digit())
    })
}

/// A stable ack key, plus the agent alias when the row names one, for
/// an overview needs-me row. The rows carry `kind`/`title`/`command`
/// but no id, so the identity is read back from the shapes `overview`
/// writes: `agent <alias> …`, `… for <alias>`, `PR #<n> …` beside
/// `--repo <slug>`, `<ISSUE-ID> …`. A shape it can't read keys on the
/// kind alone — coarse, never wrong about which row it names.
fn need_key(n: &Value) -> (String, Option<String>) {
    let kind = n["kind"].as_str().unwrap_or("needs");
    let title = n["title"].as_str().unwrap_or_default();
    let words: Vec<&str> = title.split_whitespace().collect();
    match kind {
        "inbox_unread" | "approval" => {
            if let Some((_, alias)) = title.rsplit_once(" for ") {
                let alias = alias.trim();
                let k = if kind == "inbox_unread" {
                    "inbox"
                } else {
                    kind
                };
                return (format!("{k}:{alias}"), Some(alias.to_string()));
            }
        }
        "merge" | "pr_no_verdict" => {
            let num = title
                .strip_prefix("PR #")
                .and_then(|r| r.split_whitespace().next());
            let cmd: Vec<&str> = n["command"]
                .as_str()
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            let slug = cmd
                .iter()
                .position(|w| *w == "--repo")
                .and_then(|i| cmd.get(i + 1));
            if let (Some(num), Some(slug)) = (num, slug) {
                return (format!("{kind}:{slug}#{num}"), None);
            }
        }
        "ci_red" => {
            if let Some(slug) = words.last() {
                return (format!("{kind}:{slug}"), None);
            }
        }
        _ => {}
    }
    if words.first() == Some(&"agent") {
        if let Some(alias) = words.get(1) {
            return (format!("{kind}:{alias}"), Some(alias.to_string()));
        }
    }
    if let Some(id) = words.first().filter(|w| is_issue_id(w)) {
        return (format!("{kind}:{id}"), None);
    }
    (kind.to_string(), None)
}

/// Findings outside the focus, as one summary row: count and worst
/// severity per project. Capped at `warn` — another project's backlog
/// is worth a line, never a no-go.
fn others_row(out: &[Finding], focus: &Focus) -> Option<Row> {
    if out.is_empty() {
        return None;
    }
    let mut per: Vec<(String, usize, Sev)> = Vec::new();
    for f in out {
        let p = f.project.clone().unwrap_or_default();
        match per.iter_mut().find(|(k, _, _)| *k == p) {
            Some(e) => {
                e.1 += 1;
                e.2 = e.2.max(f.sev);
            }
            None => per.push((p, 1, f.sev)),
        }
    }
    per.sort();
    let worst = per.iter().map(|e| e.2).max().unwrap_or(Sev::Ok);
    let here = match focus {
        Focus::Project(p) => p.as_str(),
        Focus::All(_) => "",
    };
    let mut row = Row::new("others");
    row.sev = worst.min(Sev::Warn);
    row.detail = format!(
        "{} item(s) outside {here}: {}",
        out.len(),
        per.iter()
            .map(|(k, n, s)| format!("{k} {n} (worst {})", s.name()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    row.remedy = Some("cadence session start --all".to_string());
    Some(row)
}

/// A check row from its keyed findings — acks applied, the ones in
/// focus kept, the rest handed back for the summary row.
fn findings_row(
    name: &'static str,
    mut findings: Vec<Finding>,
    focus: &Focus,
    acks: &Acks,
    now: i64,
    clean: &str,
    elsewhere: &mut Vec<Finding>,
) -> Row {
    for f in &mut findings {
        acks.apply(f, now);
    }
    let (mine, theirs): (Vec<Finding>, Vec<Finding>) = findings
        .into_iter()
        .partition(|f| focus.covers(f.project.as_deref()));
    elsewhere.extend(theirs);
    let mut row = Row::new(name);
    if mine.is_empty() {
        return row.ok(clean);
    }
    row.sev = mine.iter().map(|f| f.sev).max().unwrap_or(Sev::Ok);
    row.detail = format!("{} item(s)", mine.len());
    row.findings = mine;
    row
}

// ---------- session start ----------

pub struct StartOptions {
    pub project: Option<String>,
    /// `--all`: judge every project, today's fleet-wide gate.
    pub all: bool,
    pub json: bool,
    pub fix: bool,
    /// `--host-report` fixture — debug/tests only; labelled in output.
    pub host_report: Option<PathBuf>,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
}

pub fn run_start(opts: &StartOptions) -> Result<i32> {
    let mut rows: Vec<Row> = Vec::new();
    let mut fixes: Vec<String> = Vec::new();

    let now = itime::now_epoch();
    // ---- scope: every project is read; the focus decides which
    // findings can move the exit code (CAD-257) ----
    let sc = scope(None)?;
    let focus = if opts.all {
        Focus::All(None)
    } else if let Some(p) = &opts.project {
        if !sc.projects.iter().any(|x| &x.key == p) {
            return Err(Error::rejected(format!(
                "unknown project '{p}' — `cadence issue project ls` lists the keys"
            )));
        }
        Focus::Project(p.clone())
    } else {
        match cwd_project(&sc.projects, &opts.cwd) {
            Some(p) => Focus::Project(p),
            None => Focus::All(Some(format!(
                "{} is not inside a known project repo — judging every project, as --all",
                opts.cwd.display()
            ))),
        }
    };
    let acks = Acks::load(&opts.state_dir);
    if let Some(e) = &acks.error {
        rows.push(Row::new("acks").warn(
            format!("{e} — no acknowledgement applied"),
            "fix or move the file, then `cadence session ack --list`",
        ));
    }

    // ---- host: the CAD-72 watchdog — disk, WAL, pipes, orphans,
    // temp dirs, stale worktrees in one read-only scan ----
    let host_scan = host_report(
        &host_scan_for(opts.state_dir.clone(), opts.cwd.clone()),
        opts.host_report.as_deref(),
    )?;
    let host = host_row("host", &host_scan, opts.host_report.as_deref(), &acks, now);
    rows.push(host);

    // ---- binary vs main ----
    let mut bin = Row::new("binary");
    let repo_for_binary = match &opts.project {
        Some(p) => sc
            .repos
            .iter()
            .find(|(k, _, _)| k == p)
            .map(|(_, _, r)| r.clone()),
        None => overview::build_repo_match_pub(&sc.projects).map(|(_, r)| r),
    };
    match repo_for_binary {
        Some(repo) => {
            let d = overview::drift_of(&repo, overview::BUILD_COMMIT);
            if d["known"].as_bool() == Some(true) {
                let n = d["count"].as_i64().unwrap_or(0);
                bin = if n > 0 {
                    bin.warn(
                        format!(
                            "binary is {n} commit(s) behind {}",
                            d["ref"].as_str().unwrap_or("main")
                        ),
                        "git pull && cargo build --release --features ui",
                    )
                } else {
                    bin.ok(format!(
                        "at {}",
                        &overview::BUILD_COMMIT[..10.min(overview::BUILD_COMMIT.len())]
                    ))
                };
            } else {
                bin = bin.ok(d["reason"].as_str().unwrap_or("cannot tell").to_string());
            }
        }
        None => bin = bin.ok("no repo matches this build — drift unknown"),
    }
    rows.push(bin);

    // ---- daemon ----
    let mut fl = fleet(&opts.state_dir);
    if !fl.reachable && opts.fix {
        match client::daemon_start(&opts.state_dir) {
            Ok(v) => {
                fixes.push(format!(
                    "daemon start → {}",
                    v["state"].as_str().unwrap_or("?")
                ));
                fl = fleet(&opts.state_dir);
            }
            Err(e) => fixes.push(format!("daemon start failed: {e}")),
        }
    }
    let mut drow = Row::new("daemon");
    if !fl.reachable {
        drow = drow.fail("unreachable", "cadence daemon start");
    } else {
        match &fl.info {
            Some(info) => {
                let dc = info["build_commit"].as_str().unwrap_or("unknown");
                if dc != "unknown" && dc != overview::BUILD_COMMIT {
                    drow = drow.warn(
                        format!("daemon build {dc} != binary {}", overview::BUILD_COMMIT),
                        "daemon behind — run `cadence daemon restart`",
                    );
                } else {
                    let up = match uptime_mins(info, itime::now_epoch()) {
                        Some(m) => format!("up {m}m"),
                        None => "uptime unknown".to_string(),
                    };
                    drow = drow.ok(format!("{up}, {} agents", fl.agents.len()));
                }
            }
            None => {
                drow = drow.warn(
                    "daemon predates daemon_info — build unreadable",
                    "cadence daemon restart",
                );
            }
        }
    }
    if let Some(f) = fixes.last().filter(|f| f.starts_with("daemon")) {
        drow.fixed = Some(f.clone());
    }
    rows.push(drow);

    // ---- board ----
    let mut board = Row::new("board");
    let ui_opts = ui::persisted_opts(&opts.state_dir);
    let port = ui_opts.port.unwrap_or(3010);
    if ui::detached_pid(&opts.state_dir).is_none() && opts.fix {
        match ui::start_quiet(&opts.state_dir, &ui::UiFlags::default(), false) {
            Ok(_) => {
                fixes.push("ui start".to_string());
                board.fixed = Some("ui start".to_string());
            }
            Err(e) => fixes.push(format!("ui start failed: {e}")),
        }
    }
    match ui::detached_pid(&opts.state_dir) {
        None => {
            board = if board.fixed.is_some() {
                board.warn("ui start ran but no pid yet", "cadence ui status")
            } else {
                board.warn("board down", "cadence ui start")
            };
        }
        Some(pid) => {
            let mut detail = format!("pid {pid}, :{port}");
            let unhealthy = ui::health(&opts.state_dir).is_none();
            if unhealthy {
                detail.push_str(", health probe failed");
            }
            // `ts_live` is the post-fix truth: a persisted mapping that
            // is not serving lifts the row to warn even after `--fix`
            // ran something else (`board.fixed` set by `ui start` must
            // not mask a dead mapping or an errored ts_start).
            let mut ts_live = true;
            if let Some(ts) = &ui_opts.tailscale {
                let mut live = ui::serve_has_target(&ts.target).unwrap_or(false);
                if !live && opts.fix {
                    match ui::ts_start_quiet(&opts.state_dir, ts.https_port, ui_opts.read_only) {
                        Ok(_) => {
                            live = ui::serve_has_target(&ts.target).unwrap_or(false);
                            if live {
                                board.fixed = Some(format!("ui tailscale start → {}", ts.url()));
                                fixes.push(format!("ui tailscale start → {}", ts.url()));
                            } else {
                                fixes.push(
                                    "ui tailscale start ran but the mapping is still not live"
                                        .to_string(),
                                );
                            }
                        }
                        Err(e) => fixes.push(format!("ui tailscale start failed: {e}")),
                    }
                }
                ts_live = live;
                if live {
                    detail.push_str(&format!(", shared {}", ts.url()));
                } else {
                    detail.push_str(", tailscale mapping not live");
                }
            }
            board = if unhealthy {
                board.warn(detail, "cadence ui status")
            } else if !ts_live {
                board.warn(detail, "cadence ui tailscale start")
            } else {
                board.ok(detail)
            };
        }
    }
    rows.push(board);

    // ---- reconcile + inbox: keyed findings, each attributed to a
    // project where something names one ----
    let mut elsewhere: Vec<Finding> = Vec::new();
    if !fl.reachable {
        rows.push(Row::new("reconcile").fail(
            "cannot inspect agents — daemon unreachable",
            "cadence daemon start",
        ));
        rows.push(Row::new("inbox").fail("cannot read mailboxes — daemon unreachable", ""));
    } else {
        let owner_of = agent_projects(&fl.agents, &sc);
        let agent_proj = |alias: &str| owner_of.get(alias).cloned();
        let mut recon: Vec<Finding> = Vec::new();
        let mut inbox: Vec<Finding> = Vec::new();
        // Overview computes the shared needs-me rows once (gh cache
        // included); inbox rows split out into their own check.
        let pm_dir = issue::default_dir().unwrap_or_default();
        let view = overview::overview(&opts.state_dir, &pm_dir);
        let needs = view["needs_me"].as_array().cloned().unwrap_or_default();
        // Hard failures: a fenced agent and an unknown message both
        // mean a turn's outcome is unaccounted for — the gate refuses
        // go until a human reconciles (or acknowledges) them.
        // Everything else warns.
        for n in &needs {
            let kind = n["kind"].as_str().unwrap_or_default();
            let line = format!(
                "{} — {}",
                n["title"].as_str().unwrap_or_default(),
                n["command"].as_str().unwrap_or_default()
            );
            let (key, alias) = need_key(n);
            let project = n["project"]
                .as_str()
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .or_else(|| alias.as_deref().and_then(agent_proj));
            let sev = if kind == "fenced" {
                Sev::Fail
            } else {
                Sev::Warn
            };
            let f = Finding::new(key, project, sev, line);
            if kind == "inbox_unread" {
                inbox.push(f);
            } else {
                recon.push(f);
            }
        }
        // Unknown messages — a fence the overview rows don't name.
        for a in &fl.agents {
            let alias = a["alias"].as_str().unwrap_or_default();
            let show = fl.shows.get(alias).cloned().unwrap_or_default();
            let mut named = false;
            for m in show["messages"].as_array().cloned().unwrap_or_default() {
                if m["state"].as_str() == Some("unknown") {
                    named = true;
                    let id = m["id"].as_str().unwrap_or("?");
                    let head = m["body"]
                        .as_str()
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(50)
                        .collect::<String>();
                    recon.push(Finding::new(
                        format!("reconcile:{id}"),
                        agent_proj(alias),
                        Sev::Fail,
                        format!(
                            "agent {alias}: unknown message {id} ({head}) — cadence message reconcile {id} --status interrupted"
                        ),
                    ));
                }
            }
            if !named && show["unknown"].as_i64().unwrap_or(0) > 0 {
                recon.push(Finding::new(
                    format!("unknown:{alias}"),
                    agent_proj(alias),
                    Sev::Fail,
                    format!("agent {alias}: unknown message(s) — cadence agent show {alias}"),
                ));
            }
        }
        // `doing` issues with no live owner.
        for v in &sc.views {
            if v.status != "doing" {
                continue;
            }
            let id = &v.issue.front.id;
            let text = match v.issue.front.owner.as_deref() {
                None => Some(format!(
                    "{id} doing with no owner — cadence issue set {id} owner=<alias>"
                )),
                Some(owner) => {
                    let live = fl.agents.iter().any(|a| {
                        a["alias"].as_str() == Some(owner)
                            && !a["dead"].as_bool().unwrap_or(false)
                            && !matches!(
                                a["state"].as_str().unwrap_or_default(),
                                "stopped" | "stopping" | "offline"
                            )
                    });
                    (!live).then(|| {
                        format!("{id} doing but owner {owner} is not live — resume or reassign")
                    })
                }
            };
            if let Some(text) = text {
                recon.push(Finding::new(
                    format!("doing:{id}"),
                    Some(v.issue.project.clone()),
                    Sev::Warn,
                    text,
                ));
            }
        }
        // Open PR on a cadence/<wt> branch whose local worktree is gone
        // (only checked where the PR's repo has a declared checkout —
        // the worktree may legitimately live on another host).
        let (pr_branches, gh_repos) = gh_open(&opts.state_dir, &sc, false);
        for (slug, data) in &gh_repos {
            let Some((key, _, root)) = sc
                .repos
                .iter()
                .find(|(_, s, _)| s.as_deref() == Some(slug.as_str()))
            else {
                continue;
            };
            for pr in data["prs"].as_array().cloned().unwrap_or_default() {
                let Some(head) = pr["headRefName"].as_str() else {
                    continue;
                };
                let Some(name) = head.strip_prefix("cadence/") else {
                    continue;
                };
                if !root.join(".cadence").join("wt").join(name).exists() {
                    let n = pr["number"].as_i64().unwrap_or(0);
                    recon.push(Finding::new(
                        format!("pr:{slug}#{n}"),
                        Some(key.clone()),
                        Sev::Warn,
                        format!("PR #{n} branch {head} — no .cadence/wt/{name} locally"),
                    ));
                }
            }
        }
        // `.cadence/wt/*` with neither an open PR nor an open issue.
        for (root, name) in worktree_dirs(&sc.repos) {
            let branch = format!("cadence/{name}");
            let has_pr = pr_branches.contains(&branch);
            let open_issue = issue_stem(&name)
                .and_then(|id| sc.issue_status.get(&id))
                .is_some_and(|s| !matches!(s.as_str(), "done" | "dropped"));
            if !has_pr && !open_issue {
                let project = sc
                    .repos
                    .iter()
                    .find(|(_, _, r)| *r == root)
                    .map(|(k, _, _)| k.clone());
                recon.push(Finding::new(
                    format!("worktree:{}/.cadence/wt/{name}", root.display()),
                    project,
                    Sev::Warn,
                    format!(
                        "orphan worktree {}/.cadence/wt/{name} — no open PR or issue; \
                         inspect, then `git -C {} worktree remove .cadence/wt/{name}`",
                        root.display(),
                        root.display()
                    ),
                ));
            }
        }
        rows.push(findings_row(
            "reconcile",
            recon,
            &focus,
            &acks,
            now,
            "nothing stale",
            &mut elsewhere,
        ));
        rows.push(findings_row(
            "inbox",
            inbox,
            &focus,
            &acks,
            now,
            "no unread",
            &mut elsewhere,
        ));
    }
    if let Some(r) = others_row(&elsewhere, &focus) {
        rows.push(r);
    }

    finish_start(
        rows,
        &focus,
        opts.json,
        host_source(opts.host_report.as_deref()),
    )
}

fn finish_start(rows: Vec<Row>, focus: &Focus, json_out: bool, host_source: String) -> Result<i32> {
    let worst = rows.iter().map(|r| r.sev).max().unwrap_or(Sev::Ok);
    let exit = match worst {
        Sev::Ok => 0,
        Sev::Warn => 1,
        Sev::Fail => 2,
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "session-start",
                "go": worst != Sev::Fail,
                "scope": focus.json(),
                "host_source": host_source,
                "checks": rows.iter().map(|r| r.json()).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        );
    } else {
        let label = match worst {
            Sev::Ok => "GO",
            Sev::Warn => "GO (with warnings)",
            Sev::Fail => "NO-GO",
        };
        println!("session start — {label}");
        match focus {
            Focus::Project(p) => {
                println!("scope: project {p} — other projects summarised; --all for the fleet")
            }
            Focus::All(None) => println!("scope: every project (--all)"),
            Focus::All(Some(note)) => println!("scope: {}", scrub_line(note)),
        }
        for r in &rows {
            print_row(r);
        }
        let failing = rows.iter().any(|r| {
            (r.sev == Sev::Fail && r.key.is_some()) || r.findings.iter().any(|f| f.sev == Sev::Fail)
        });
        if failing {
            println!(
                "known and parked? cadence session ack <key> --reason <why> --expires <3d, max 14d>"
            );
        }
    }
    Ok(exit)
}

// ---------- session end ----------

pub struct EndOptions {
    pub project: Option<String>,
    pub json: bool,
    pub dry_run: bool,
    pub force_finish: bool,
    pub idle_secs: u64,
    /// `--host-report` fixture — debug/tests only; labelled in output.
    pub host_report: Option<PathBuf>,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
}

pub fn run_end(opts: &EndOptions) -> Result<i32> {
    let now = itime::now_epoch();
    let sc = scope(opts.project.as_deref())?;
    // A `--host-report` fixture is loaded (and validated) before any
    // sweep, stop or gc can run — a bad path must not error *after*
    // the mutations it was meant to observe.
    let host_scan = host_report(
        &host_scan_for(opts.state_dir.clone(), opts.cwd.clone()),
        opts.host_report.as_deref(),
    )?;
    let fl = fleet(&opts.state_dir);
    let mut rows: Vec<Row> = Vec::new();
    let mut done = EndActions::default();
    let mut failures = 0u32;

    // ---- merged-worktree sweep: the library call, not the CLI —
    // nothing prints ahead of a --json report, --project scopes it,
    // and its own dry run is the plan ----
    let mut sweep = Row::new("finish");
    match issue::Pm::open_default() {
        Err(_) => {
            sweep = sweep.ok("no tracker — nothing to finish");
        }
        Ok(pm) => match issue::finish::sweep(
            &pm,
            opts.project.as_deref(),
            false,
            opts.dry_run,
            "",
            &opts.state_dir,
        ) {
            Ok(out) => {
                let srows = out["rows"].as_array().cloned().unwrap_or_default();
                let (mut refused, mut would) = (0usize, 0usize);
                for r in &srows {
                    let wt = r["worktree"].as_str().unwrap_or("?");
                    let branch = r["branch"].as_str().unwrap_or("?");
                    let label = format!("{wt} ({branch})");
                    let reason = r["reason"]
                        .as_str()
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    match r["outcome"].as_str().unwrap_or_default() {
                        "finished" => {
                            done.finished.push(wt.to_string());
                            sweep.items.push(format!("{label} — finished"));
                        }
                        "would-finish" => {
                            would += 1;
                            sweep.items.push(format!("{label} — would finish"));
                        }
                        "skipped" => {
                            sweep.items.push(format!("{label} — skipped({reason})"));
                        }
                        "refused" => {
                            refused += 1;
                            sweep.items.push(format!("{label} — refused({reason})"));
                        }
                        _ => {}
                    }
                }
                sweep.detail = if opts.dry_run {
                    format!(
                        "would run `cadence issue finish --merged` — {} candidate(s)",
                        would + refused
                    )
                } else {
                    // The same candidate set the dry run counts —
                    // finished + refused; `skipped` rows were never
                    // candidates, so they can't join the denominator.
                    format!(
                        "{} finished of {} candidate(s)",
                        done.finished.len(),
                        done.finished.len() + refused
                    )
                };
                if refused > 0 {
                    sweep.sev = Sev::Warn;
                }
            }
            Err(e) => {
                sweep = sweep.fail(format!("issue finish --merged failed: {e}"), "");
                failures += 1;
            }
        },
    }
    // --force-finish honesty: `issue finish --merged` has no --force
    // (the flag conflicts with --merged outright) — say so rather than
    // reporting a force that never ran.
    if opts.force_finish {
        sweep
            .items
            .push("--force-finish ignored: issue finish --merged has no --force".to_string());
        // The note lifts an otherwise-clean sweep to warn; a Fail
        // stays Fail — downgrading it for display would lie.
        if sweep.sev == Sev::Ok {
            sweep.sev = Sev::Warn;
        }
        done.finish_notes
            .push("--force-finish ignored: issue finish --merged has no --force".to_string());
    }
    rows.push(sweep);

    // ---- idle agents ----
    // --project scopes the stop sweep to that project's agents: the
    // owners of its issues plus any agent whose cwd lives under one of
    // its repo checkouts — everything else is left running.
    let owners: HashSet<&str> = sc
        .views
        .iter()
        .filter_map(|v| v.issue.front.owner.as_deref())
        .collect();
    let in_scope = |a: &Value| -> bool {
        if opts.project.is_none() {
            return true;
        }
        let alias = a["alias"].as_str().unwrap_or_default();
        if owners.contains(alias) {
            return true;
        }
        let cwd = a["cwd"].as_str().unwrap_or_default();
        !cwd.is_empty()
            && sc.repos.iter().any(|(_, _, p)| {
                // An empty declared repo path starts_with()s every
                // cwd — that would scope the whole fleet.
                !p.as_os_str().is_empty() && Path::new(cwd).starts_with(p)
            })
    };
    let mut idle_row = Row::new("agents");
    let mut stop_candidates: Vec<String> = Vec::new();
    let mut busy = 0u32;
    let mut out_of_scope = 0u32;
    if fl.reachable {
        for a in &fl.agents {
            let alias = a["alias"].as_str().unwrap_or_default();
            let provider = a["provider"].as_str().unwrap_or_default();
            let kind = a["endpoint_kind"].as_str().unwrap_or_default();
            if !registry::has_actor(provider, kind) {
                continue;
            }
            if !in_scope(a) {
                if a["state"].as_str() == Some("idle") {
                    out_of_scope += 1;
                }
                continue;
            }
            let show = fl.shows.get(alias).cloned().unwrap_or_default();
            match stoppable(&opts.state_dir, a, &show, opts.idle_secs, now) {
                Stop::Yes => stop_candidates.push(alias.to_string()),
                Stop::Busy => busy += 1,
                Stop::No => {}
            }
        }
    }
    if !fl.reachable {
        idle_row = idle_row.warn("daemon unreachable — nothing stopped", "");
    } else if stop_candidates.is_empty() {
        idle_row = idle_row.ok(format!(
            "nothing idle past {}s ({busy} busy)",
            opts.idle_secs
        ));
    } else {
        idle_row = Row::new("agents").ok(format!(
            "{} agent(s) idle > {}s{}",
            stop_candidates.len(),
            opts.idle_secs,
            if opts.dry_run { " (dry-run)" } else { "" }
        ));
        idle_row.items = stop_candidates.clone();
        if !opts.dry_run {
            for (i, alias) in stop_candidates.iter().enumerate() {
                // The fleet snapshot is stale — the finish sweep ran
                // git/gh per worktree. Re-verify against a live
                // agent_show before stopping: an agent that claimed
                // work meanwhile is skipped, never killed mid-turn.
                let still = client::rpc(&opts.state_dir, "agent_show", json!({"alias": alias}))
                    .map(|show| {
                        matches!(
                            stoppable(
                                &opts.state_dir,
                                &show["agent"],
                                &show,
                                opts.idle_secs,
                                itime::now_epoch(),
                            ),
                            Stop::Yes
                        )
                    })
                    .unwrap_or(false);
                if !still {
                    done.stop_skipped.push(alias.clone());
                    idle_row.items[i] =
                        format!("{alias} — skipped: busy or changed during the run");
                    continue;
                }
                match client::rpc(&opts.state_dir, "agent_stop", json!({"alias": alias})) {
                    Ok(_) => done.stopped.push(alias.clone()),
                    Err(e) => {
                        idle_row.items[i] = format!("{alias} — stop failed: {e}");
                        failures += 1;
                    }
                }
            }
        }
    }
    if out_of_scope > 0 {
        idle_row.items.push(format!(
            "{out_of_scope} idle agent(s) outside project '{}' left alone",
            opts.project.as_deref().unwrap_or_default()
        ));
    }
    rows.push(idle_row);

    // ---- agent gc ----
    let mut gc_row = Row::new("gc");
    if !fl.reachable {
        gc_row = gc_row.ok("skipped — daemon unreachable");
    } else if let Some(p) = opts.project.as_deref() {
        // `agent_gc` has no scope parameter — it sweeps the whole
        // fleet. Under --project that's another project's agents, so
        // skip it and say so rather than overreach.
        gc_row = gc_row.ok(format!("gc is fleet-wide — skipped under --project {p}"));
    } else if opts.dry_run {
        let cands: Vec<String> = fl
            .agents
            .iter()
            .filter(|a| {
                a["endpoint"].is_null()
                    && matches!(a["state"].as_str(), Some("attention" | "stopped"))
                    && now - a["updated"].as_f64().unwrap_or(now as f64) as i64 > 3600
            })
            .filter_map(|a| a["alias"].as_str().map(str::to_string))
            .collect();
        gc_row = gc_row.ok(format!("would sweep {} dead agent(s)", cands.len()));
        gc_row.items = cands;
    } else {
        match client::rpc(&opts.state_dir, "agent_gc", json!({"older_than": 3600})) {
            Ok(v) => {
                done.gc_removed = v["removed"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect();
                gc_row = gc_row.ok(format!("removed {}", done.gc_removed.len()));
            }
            Err(e) => {
                gc_row = gc_row.fail(format!("agent gc failed: {e}"), "");
                failures += 1;
            }
        }
    }
    rows.push(gc_row);

    // ---- host sweep: the CAD-72 watchdog again — orphans are
    // reported, never killed; disk state rides along. A report, not
    // a gate: a full disk is `warn` here — `session start` is where a
    // full host is correctly `fail` — so only this verb's own failures
    // (a sweep RPC error, an unwritable handoff) set exit 2. ----
    let acks = Acks::load(&opts.state_dir);
    let mut sweep_row = host_row("sweep", &host_scan, opts.host_report.as_deref(), &acks, now);
    if sweep_row.sev == Sev::Fail {
        sweep_row.sev = Sev::Warn;
    }
    if sweep_row.detail == "host clean" {
        sweep_row.detail = "clean".to_string();
    }
    rows.push(sweep_row);

    // ---- handoff: --dry-run writes nothing — the row names the file
    // it would write and the markdown goes to stdout / the json
    // payload. Real runs use a timestamped name and never overwrite.
    let (_, gh_repos) = gh_open(&opts.state_dir, &sc, opts.dry_run);
    let md = handoff_md(opts, &fl, &sc, &gh_repos, &done, now);
    let mut handoff_path = PathBuf::new();
    let mut handoff_preview = None;
    if opts.dry_run {
        let would = opts.state_dir.join("sessions").join(handoff_name(now, 0));
        rows.push(Row::new("handoff").ok(format!("would write {}", would.display())));
        handoff_preview = Some(md.clone());
    } else {
        let dir = opts.state_dir.join("sessions");
        let path = (0..100u32)
            .map(|n| dir.join(handoff_name(now, n)))
            .find(|p| !p.exists());
        match path {
            None => {
                rows.push(Row::new("handoff").fail("no free handoff filename", ""));
                failures += 1;
            }
            Some(p) => match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&p, &md)) {
                Ok(_) => handoff_path = p,
                Err(e) => {
                    rows.push(Row::new("handoff").fail(format!("{e}"), ""));
                    failures += 1;
                }
            },
        }
    }
    if !handoff_path.as_os_str().is_empty() {
        rows.push(Row::new("handoff").ok(handoff_path.display().to_string()));
    }

    let worst = rows.iter().map(|r| r.sev).max().unwrap_or(Sev::Ok);
    let mut exit = match worst {
        Sev::Ok => 0,
        Sev::Warn => 1,
        Sev::Fail => 2,
    };
    if failures > 0 {
        exit = 2;
    }
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "session-end",
                "dry_run": opts.dry_run,
                "host_source": host_source(opts.host_report.as_deref()),
                "steps": rows.iter().map(|r| r.json()).collect::<Vec<_>>(),
                "stop_candidates": stop_candidates,
                "stopped": done.stopped,
                "stop_skipped": done.stop_skipped,
                "gc_removed": done.gc_removed,
                "handoff": handoff_path,
                "handoff_md": if opts.dry_run { Some(md) } else { None },
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "session end{}",
            if opts.dry_run { " — dry run" } else { "" }
        );
        for r in &rows {
            print_row(r);
        }
        if let Some(preview) = &handoff_preview {
            println!("\n{preview}");
        }
    }
    Ok(exit)
}

/// What `session end` applied — reported on screen and in the handoff.
#[derive(Default)]
struct EndActions {
    stopped: Vec<String>,
    /// Candidates that went busy between the fleet snapshot and their
    /// pre-stop re-check — skipped, never killed mid-turn.
    stop_skipped: Vec<String>,
    gc_removed: Vec<String>,
    finished: Vec<String>,
    /// Honest finish notes (e.g. `--force-finish` ignored).
    finish_notes: Vec<String>,
}

/// Daemon uptime in whole minutes from `daemon_info`. `started_at` is
/// fractional epoch seconds, so it is read as f64; a missing or future
/// value is unknown rather than measured from the epoch.
fn uptime_mins(info: &Value, now: i64) -> Option<i64> {
    let started = info["started_at"].as_f64().filter(|s| *s > 0.0)? as i64;
    (now >= started).then(|| (now - started) / 60)
}

/// The handoff filename — timestamped so two runs the same day never
/// overwrite each other; `n` disambiguates same-second runs.
fn handoff_name(now: i64, n: u32) -> String {
    let (y, mo, d, h, mi, s) = itime::utc_parts(now);
    if n == 0 {
        format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z-end.md")
    } else {
        format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z-{n}-end.md")
    }
}

/// The end-of-day note: open PRs with head + verdict, live turns,
/// queued work, issues in review, and what the next session does first.
/// Pure — the caller decides whether (and where) it lands on disk.
fn handoff_md(
    opts: &EndOptions,
    fl: &Fleet,
    sc: &Scope,
    gh_repos: &HashMap<String, Value>,
    done: &EndActions,
    now: i64,
) -> String {
    let (y, mo, d, h, mi, s) = itime::utc_parts(now);
    let mut md = format!("# session end — {y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z\n\n");

    md.push_str("## open PRs\n");
    let mut any_pr = false;
    for (slug, data) in gh_repos {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            any_pr = true;
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let verdict = overview::verdict_state_pub(&rollup).unwrap_or_else(|| "none".into());
            let checks = if overview::checks_green_pub(&rollup) {
                "green"
            } else {
                "not-green"
            };
            md.push_str(&format!(
                "- {slug}#{} {} — head {}, verdict {}, checks {}\n",
                pr["number"].as_i64().unwrap_or(0),
                scrub_line(pr["title"].as_str().unwrap_or("")),
                pr["headRefOid"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(10)
                    .collect::<String>(),
                verdict,
                checks,
            ));
        }
    }
    if !any_pr {
        md.push_str("- none\n");
    }

    md.push_str("\n## running turns\n");
    let mut any_run = false;
    let mut queued_lines = String::new();
    for a in &fl.agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        if done.stopped.iter().any(|s| s == alias) {
            continue;
        }
        // Post-sweep truth, not the pre-sweep snapshot: an agent that
        // claimed work during the run (the `stop_skipped` list) shows
        // here as running, and one we just stopped is absent.
        let show = if opts.dry_run {
            fl.shows.get(alias).cloned().unwrap_or_default()
        } else {
            client::rpc(&opts.state_dir, "agent_show", json!({"alias": alias}))
                .unwrap_or_else(|_| fl.shows.get(alias).cloned().unwrap_or_default())
        };
        if let Some((id, head, age)) = running_msg(&show, now) {
            any_run = true;
            md.push_str(&format!("- {alias}: {id} ({age}s) {}\n", scrub_line(&head)));
        }
        let queued = show["queued"].as_i64().unwrap_or(0);
        if queued > 0 {
            queued_lines.push_str(&format!("- {alias}: {queued} queued\n"));
        }
    }
    if !any_run {
        md.push_str("- none\n");
    }
    md.push_str("\n## queued kickoffs\n");
    md.push_str(if queued_lines.is_empty() {
        "- none\n"
    } else {
        &queued_lines
    });

    md.push_str("\n## issues in review\n");
    let review: Vec<&str> = sc
        .views
        .iter()
        .filter(|v| v.status == "review")
        .map(|v| v.issue.front.id.as_str())
        .collect();
    if review.is_empty() {
        md.push_str("- none\n");
    } else {
        for id in review {
            md.push_str(&format!("- {id}\n"));
        }
    }

    md.push_str("\n## done this run\n");
    md.push_str(&format!(
        "- stopped: {}\n",
        if done.stopped.is_empty() {
            "none".into()
        } else {
            done.stopped.join(", ")
        }
    ));
    if !done.stop_skipped.is_empty() {
        md.push_str(&format!(
            "- skipped (went busy during the run): {}\n",
            done.stop_skipped.join(", ")
        ));
    }
    md.push_str(&format!(
        "- gc removed: {}\n",
        if done.gc_removed.is_empty() {
            "none".into()
        } else {
            done.gc_removed.join(", ")
        }
    ));
    md.push_str(&format!(
        "- worktrees finished: {}\n",
        if done.finished.is_empty() {
            "none".into()
        } else {
            done.finished.join(", ")
        }
    ));
    for n in &done.finish_notes {
        md.push_str(&format!("- {n}\n"));
    }
    if opts.dry_run {
        md.push_str("- dry run — nothing above was applied\n");
    }

    md.push_str("\n## next session first\n");
    let pm_dir = issue::default_dir().unwrap_or_default();
    // Dry-run: the cache-only overview — a dry run writes nothing,
    // cache included.
    let view = if opts.dry_run {
        overview::overview_cached(&opts.state_dir, &pm_dir)
    } else {
        overview::overview(&opts.state_dir, &pm_dir)
    };
    let needs = view["needs_me"].as_array().cloned().unwrap_or_default();
    if needs.is_empty() {
        md.push_str("- nothing queued on a human\n");
    } else {
        for n in needs.iter().take(5) {
            md.push_str(&format!(
                "- {} — {}\n",
                scrub_line(n["title"].as_str().unwrap_or("")),
                scrub_line(n["command"].as_str().unwrap_or(""))
            ));
        }
    }
    let live = fl
        .agents
        .iter()
        .filter(|a| !a["dead"].as_bool().unwrap_or(false))
        .count();
    md.push_str(&format!(
        "\n## fleet\n- daemon {}: {} agent(s) live\n",
        if fl.reachable {
            "reachable"
        } else {
            "unreachable"
        },
        live
    ));
    if fl.reachable && live == 0 {
        md.push_str("- no live agents — `cadence daemon stop` is safe\n");
    } else if fl.reachable {
        md.push_str("- agents live — daemon left running\n");
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_fractional_started_at() {
        // daemon_info serializes started_at as f64 epoch seconds; an
        // integer read yields None and used to measure from 1970.
        let info = json!({"started_at": 1_000_000.75});
        assert_eq!(uptime_mins(&info, 1_000_000 + 125), Some(2));
        assert_eq!(uptime_mins(&json!({}), 1_000_000), None);
        assert_eq!(uptime_mins(&json!({"started_at": 0.0}), 1_000_000), None);
        assert_eq!(uptime_mins(&info, 999_000), None);
    }

    // These pin the session-side boundary — every display line goes
    // through `scrub_line` before it renders or serializes.
    // `scrub_line` is `scrub_auth_spans` (key<sep>value expressions
    // argv-tokenization can't see) feeding `doctor::host::redact_argv`
    // (CAD-108's shared per-token scrubber) plus the docker-login
    // positional.

    #[test]
    fn scrub_line_redacts_flag_env_and_bare_shapes() {
        // The observed figma argv — flag=value form.
        assert_eq!(
            scrub_line("npm exec figma-developer-mcp --figma-api-key=figd_ABC123 --stdio"),
            "npm exec figma-developer-mcp --figma-api-key=[REDACTED] --stdio"
        );
        // `--flag value`, `NAME=value`, and a bare credential token
        // with no flag name at all — the shape catches it.
        for (line, want) in [
            (
                "cmd --token s3cr3t --verbose",
                "cmd --token [REDACTED] --verbose",
            ),
            (
                "env GITHUB_TOKEN=ghp_XYZ run",
                "env GITHUB_TOKEN=[REDACTED] run",
            ),
            (
                "pid 9 leaked figd_TESTTOKEN00000000000000000000000 here",
                "pid 9 leaked [REDACTED] here",
            ),
            ("auth: Bearer ghp_abcdefghijklmnopqrst", "auth: [REDACTED]"),
            // AWS access-key id and JWT shapes, no keyword at all.
            (
                "leaked AKIAIOSFODNN7EXAMPLE in argv",
                "leaked [REDACTED] in argv",
            ),
            (
                "head eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.rTwpG2U8x9 tail",
                "head [REDACTED] tail",
            ),
        ] {
            assert_eq!(scrub_line(line), want, "{line}");
        }
        // Ordinary text passes through untouched.
        let keep = "disk: /tmp 12.4% free (30.6 GiB) — du -xh --max-depth=1 /tmp | sort -h";
        assert_eq!(scrub_line(keep), keep);
    }

    #[test]
    fn scrub_line_redacts_auth_schemes_and_quoted_values() {
        // The observed leak: a non-Bearer scheme left the credential
        // after the masked scheme word. The whole `scheme credential`
        // expression is masked now, whatever the scheme.
        for (line, want) in [
            (
                "curl -H Authorization: Basic dXNlcjpwYXNzd29yZA== https://api.example.com",
                "curl -H Authorization: [REDACTED] https://api.example.com",
            ),
            (
                "Authorization: Token abc123def456",
                "Authorization: [REDACTED]",
            ),
            ("Authorization: ApiKey zzz", "Authorization: [REDACTED]"),
            ("Authorization: Negotiate YlBJ", "Authorization: [REDACTED]"),
            // Quoted value: mask to the close quote, keep the quotes.
            (
                "cmd --auth-token \"Bearer sk-live-abc123\" --verbose",
                "cmd --auth-token \"[REDACTED]\" --verbose",
            ),
            ("token 'sekret v2' done", "token '[REDACTED]' done"),
            // `key:`/`key =` text separators.
            ("secret_key: hunter2 rest", "secret_key: [REDACTED] rest"),
            ("api-key = abc123 rest", "api-key = [REDACTED] rest"),
        ] {
            assert_eq!(scrub_line(line), want, "{line}");
        }
    }

    #[test]
    fn scrub_line_covers_uri_env_glued_and_positional_shapes() {
        for (line, want) in [
            // Attached short flag — mysql's own argv shape.
            ("mysql -uroot -pHunter2 db", "mysql -uroot -p[REDACTED] db"),
            // URI userinfo keeps the user, masks the password.
            (
                "git clone https://alice:s3cr3t@github.com/x/y",
                "git clone https://alice:[REDACTED]@github.com/x/y",
            ),
            // Glued env names — no separator before the keyword.
            (
                "env PGPASSWORD=hunter2 psql",
                "env PGPASSWORD=[REDACTED] psql",
            ),
            ("env MYSQL_PWD=s3cr3t db", "env MYSQL_PWD=[REDACTED] db"),
            ("GITHUBTOKEN=tok123 deploy", "GITHUBTOKEN=[REDACTED] deploy"),
            // `docker login`'s password is positional — no keyword
            // names it and a plain value carries no shape.
            (
                "docker login -u me s3cr3tvalue",
                "docker login -u me [REDACTED]",
            ),
            (
                "docker login --user me s3cr3tvalue",
                "docker login --user me [REDACTED]",
            ),
        ] {
            assert_eq!(scrub_line(line), want, "{line}");
        }
        // A registry argument after `-u <user>` is indistinguishable
        // from the positional password — masking wins, safe side.
        assert_eq!(
            scrub_line("docker login -u me ghcr.io"),
            "docker login -u me [REDACTED]"
        );
    }

    #[test]
    fn scrub_line_space_separator_masks_only_secret_values() {
        // Prose with a keyword in it must not lose the next word.
        for keep in [
            "token is expired",
            "pytest tests/auth test_x",
            "auth refresh flow finished",
            "credential stuffing attacks",
        ] {
            assert_eq!(scrub_line(keep), keep, "{keep}");
        }
        for (line, want) in [
            ("token hunter2", "token [REDACTED]"),
            ("auth Bearer ghp_abcdefghijklmnopqrst", "auth [REDACTED]"),
            // `=`/`:` imply a value — unconditional.
            ("password: hunter2", "password: [REDACTED]"),
            ("password = hunter2", "password = [REDACTED]"),
        ] {
            assert_eq!(scrub_line(line), want, "{line}");
        }
    }

    #[test]
    fn scrub_line_whitespace_blobs_mask_whole() {
        // CAD-141: a quoted run is one argv element — a secret inside
        // it masks the whole blob, never just the shaped word.
        for (line, want) in [
            // `flag value` inside one element — the span pass masks
            // the value, the flag name stays (same convention as
            // `--token [REDACTED]`).
            (
                "cmd \"--password hunter2\" rest",
                "cmd \"--password [REDACTED]\" rest",
            ),
            // Header shape whose left side is no keyword.
            (
                "curl -H \"X-Custom: figd_abc123 tail\"",
                "curl -H \"[REDACTED]\"",
            ),
            // A bare shaped token inside quotes (the quote would
            // defeat the argv charset check).
            ("x \"figd_secret0000\" y", "x \"[REDACTED]\" y"),
            // Quoted prose stays prose.
            (
                "say \"token is expired\" twice",
                "say \"token is expired\" twice",
            ),
            // An unmatched quote is literal text — `don't` is one word.
            ("don't split on apostrophes", "don't split on apostrophes"),
        ] {
            assert_eq!(scrub_line(line), want, "{line}");
        }
    }

    #[test]
    fn scrub_line_preserves_whitespace_when_nothing_masks() {
        let keep = "a  b\tc   indented\ttext";
        assert_eq!(scrub_line(keep), keep);
        // The span pass edits in place — a masked line keeps its
        // whitespace unless the argv pass had more to say.
        assert_eq!(
            scrub_line("a  b --token=hunter2x"),
            "a  b --token=[REDACTED]"
        );
    }

    #[test]
    fn scrub_line_keyword_needs_word_boundaries() {
        // Keywords inside ordinary words must not cost the next word —
        // the unbounded scan ate it (`monkey`, `author`, `keystore`).
        for keep in [
            "the monkey ate the sandwich",
            "the author wrote the docs",
            "keystore files on disk",
        ] {
            assert_eq!(scrub_line(keep), keep, "{keep}");
        }
    }

    #[test]
    fn scrub_line_multibyte_is_safe() {
        let out = scrub_line("pröc --token=tök rest");
        assert!(out.contains("[REDACTED]") && !out.contains("tök"));
    }

    #[test]
    fn exe_of_strips_shell_strings_to_the_executable() {
        // argv0 is a path → basename.
        assert_eq!(exe_of("/usr/bin/bash"), "bash");
        // argv0 is one shell string holding the whole command — the
        // real leak: `npm exec --figma-api-key=figd_… --stdio` as a
        // single argv element must display as `npm`, never the args.
        assert_eq!(exe_of("npm exec --api-key=figd_x --stdio"), "npm");
        assert_eq!(exe_of("sh -c 'do --secret=1 thing'"), "sh");
        assert_eq!(exe_of(""), "(unknown)");
    }

    #[test]
    fn row_json_redacts_detail_remedy_fixed_and_items() {
        let mut r = Row::new("sweep").warn("orphan --token=abc123", "kill --secret=hunter2");
        r.fixed = Some("restarted --api-key=zzz".into());
        r.items = vec!["pid 9 --secret=hunter2".into(), "clean".into()];
        let j = r.json();
        assert_eq!(j["detail"], "orphan --token=[REDACTED]");
        assert_eq!(j["remedy"], "kill --secret=[REDACTED]");
        assert_eq!(j["fixed"], "restarted --api-key=[REDACTED]");
        assert_eq!(j["items"][0], "pid 9 --secret=[REDACTED]");
        assert_eq!(j["items"][1], "clean");
    }

    fn ack(key: &str, expires: i64) -> Ack {
        Ack {
            key: key.into(),
            reason: "known".into(),
            actor: "t".into(),
            created: itime::iso(0),
            expires: itime::iso(expires),
        }
    }

    #[test]
    fn ack_downgrades_fail_until_expiry_then_fails_again() {
        let now = 1_800_000_000;
        let acks = Acks {
            list: vec![ack("reconcile:m1", now + 60)],
            error: None,
        };
        let fresh = || Finding::new("reconcile:m1".into(), None, Sev::Fail, "lost".into());
        let mut f = fresh();
        acks.apply(&mut f, now);
        assert!(f.sev == Sev::Warn);
        assert!(f.ack.as_deref().unwrap().starts_with("(acknowledged until"));
        // Same record, clock past its expiry: fail again, still named.
        let mut f = fresh();
        acks.apply(&mut f, now + 61);
        assert!(f.sev == Sev::Fail);
        assert!(f
            .ack
            .as_deref()
            .unwrap()
            .starts_with("(acknowledgement expired"));
        // Another key is untouched; a warn stays warn under an ack.
        let mut g = Finding::new("host:disk".into(), None, Sev::Fail, "x".into());
        acks.apply(&mut g, now);
        assert!(g.sev == Sev::Fail && g.ack.is_none());
        let mut w = Finding::new("reconcile:m1".into(), None, Sev::Warn, "x".into());
        acks.apply(&mut w, now);
        assert!(w.sev == Sev::Warn && w.ack.is_some());
        // The latest expiry decides — a stale record can't mask a
        // fresh one, and vice versa.
        let both = Acks {
            list: vec![ack("reconcile:m1", now - 10), ack("reconcile:m1", now + 10)],
            error: None,
        };
        let mut f = fresh();
        both.apply(&mut f, now);
        assert!(f.sev == Sev::Warn);
    }

    #[test]
    fn expiry_parses_durations_and_timestamps_within_14_days() {
        let now = 1_800_000_000;
        assert_eq!(parse_expiry("3d", now).unwrap(), now + 3 * 86_400);
        assert_eq!(parse_expiry("90m", now).unwrap(), now + 5_400);
        assert_eq!(parse_expiry("14d", now).unwrap(), now + 14 * 86_400);
        assert_eq!(
            parse_expiry(&itime::iso(now + 3_600), now).unwrap(),
            now + 3_600
        );
        for bad in ["15d", "0", "-1h", "soon", &itime::iso(now - 1)] {
            assert!(parse_expiry(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn other_projects_collapse_and_cap_at_warn() {
        let f = |key: &str, p: Option<&str>, sev| {
            Finding::new(key.into(), p.map(str::to_string), sev, key.into())
        };
        let items = || {
            vec![
                f("worktree:/a", Some("tst"), Sev::Warn),
                f("reconcile:m-oth", Some("oth"), Sev::Fail),
                f("worktree:/b", Some("oth"), Sev::Warn),
                // Unattributed stays in scope — fail closed.
                f("fenced:w9", None, Sev::Fail),
            ]
        };
        let none = Acks::default();
        let focus = Focus::Project("tst".into());
        let mut elsewhere = Vec::new();
        let row = findings_row("reconcile", items(), &focus, &none, 0, "", &mut elsewhere);
        let keys: Vec<&str> = row.findings.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["worktree:/a", "fenced:w9"]);
        assert!(row.sev == Sev::Fail);
        let others = others_row(&elsewhere, &focus).unwrap();
        assert!(others.sev == Sev::Warn, "capped at warn");
        assert!(
            others.detail.contains("oth 2 (worst fail)"),
            "{}",
            others.detail
        );
        // --all: everything judged, nothing summarised.
        let mut elsewhere = Vec::new();
        let row = findings_row(
            "reconcile",
            items(),
            &Focus::All(None),
            &none,
            0,
            "",
            &mut elsewhere,
        );
        assert_eq!(row.findings.len(), 4);
        assert!(others_row(&elsewhere, &Focus::All(None)).is_none());
    }

    #[test]
    fn need_keys_read_identity_from_overview_rows() {
        let row = |kind: &str, title: &str, command: &str| {
            need_key(&json!({"kind": kind, "title": title, "command": command}))
        };
        assert_eq!(
            row("fenced", "agent w1 fenced — reconcile then resume", "x"),
            ("fenced:w1".into(), Some("w1".into()))
        );
        assert_eq!(
            row("inbox_unread", "644 unread for fable-cc", "x"),
            ("inbox:fable-cc".into(), Some("fable-cc".into()))
        );
        assert_eq!(
            row(
                "pr_no_verdict",
                "PR #12 fix for thing — no verdict",
                "gh pr view 12 --repo o/r"
            ),
            ("pr_no_verdict:o/r#12".into(), None)
        );
        assert_eq!(
            row("intake", "CAD-3 idea report — for everyone", "x"),
            ("intake:CAD-3".into(), None)
        );
        assert_eq!(
            row("ci_red", "default branch CI failing on o/r", "x"),
            ("ci_red:o/r".into(), None)
        );
        assert_eq!(
            row("drift", "3 merged commit(s) not running", "x").0,
            "drift"
        );
    }
}
