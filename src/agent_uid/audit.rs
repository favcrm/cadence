//! `cadence agent-uid doctor` — the T1 audit: every §5 artifact, then
//! §4 rule 2's two negative assertions.
//!
//! The artifact rows reuse `provision`'s own actions as the spec — the
//! audit cannot drift from what provision installs because both read
//! the same plan. Absent artifacts are `warn` while the host shows no
//! sign of provisioning at all (a fresh host is a fact, not a fault)
//! and `fail` once any piece exists — a half-provisioned boundary is
//! worse than none.
//!
//! The negative rows are the load-bearing ones:
//!
//! - `home-acl`: nothing under the operator's home grants the agent
//!   principals access — no ACL entry naming the agent uid or its
//!   groups, and the home root itself keeps `other` empty and carries
//!   no extended ACL. §4 rejected traverse-ACLs for a reason; this is
//!   the check that keeps them rejected.
//! - `git-config`: no uid-1000 git config — `~/.gitconfig`,
//!   `~/.config/git/config`, `GIT_CONFIG_GLOBAL`, the system file, or
//!   anything an `include.path` chain pulls in — carries a
//!   `safe.directory` covering `/var/lib/cadence` or an `include.path`
//!   reaching into it. One such line is the whole agent→operator
//!   code-exec channel §4 exists to close.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::Result;

use super::{
    provision, AgentPrincipals, LiveHost, Principal, User, View, AGENT_HOME, AGENT_USER,
    HELPER_DEST, HOME_ACL_WALK_BUDGET, LAUNCH_GROUP, NOLOGIN, OPT_ROOT, SHARED_GROUP, VAR_LIB,
};

/// `ok | warn | fail`, mirroring `doctor --host` semantics — the
/// summary row there maps 1:1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "fail",
        }
    }
}

/// One row of the audit — a check name, its verdict, the measured
/// value, a human detail and the operator's remedy.
pub struct Row {
    pub name: &'static str,
    /// Negative rows assert §4 rule 2 — they fail even when nothing is
    /// provisioned yet, because they arm the channel the moment it is.
    pub negative: bool,
    pub level: Level,
    pub value: Value,
    pub detail: String,
    pub remedy: String,
}

pub struct Audit {
    pub rows: Vec<Row>,
    /// Any §5 artifact exists — the account, the share root or the
    /// helper. Drives the absent-means-warn/-fail split.
    pub provisioned: bool,
}

impl Audit {
    pub fn level(&self) -> Level {
        self.rows.iter().map(|r| r.level).max().unwrap_or(Level::Ok)
    }
    /// Rows that asserted a negative and found it *broken* — a warn
    /// (couldn't fully check) does not count as an armed violation.
    pub fn violations(&self) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|r| r.negative && r.level == Level::Fail)
            .collect()
    }
    fn failing_names(&self) -> Vec<&'static str> {
        self.rows
            .iter()
            .filter(|r| r.level == Level::Fail)
            .map(|r| r.name)
            .collect()
    }
}

fn row(
    name: &'static str,
    negative: bool,
    level: Level,
    value: Value,
    detail: String,
    remedy: String,
) -> Row {
    Row {
        name,
        negative,
        level,
        value,
        detail,
        remedy,
    }
}

const PROVISION_REMEDY: &str =
    "run `sudo cadence agent-uid provision` — the verb repairs drift idempotently";

/// The whole audit against a view. `operator` is the uid-1000 seat the
/// boundary protects.
pub fn audit(view: &dyn View, operator: &str) -> Audit {
    let agent = view.user(AGENT_USER).ok().flatten();
    let provisioned = agent.is_some()
        || view.stat(VAR_LIB).ok().flatten().is_some()
        || view.stat(HELPER_DEST).ok().flatten().is_some();
    let principals = AgentPrincipals::resolve(view).unwrap_or(AgentPrincipals {
        uid: None,
        gids: Vec::new(),
        group_names: [AGENT_USER, SHARED_GROUP],
    });

    let spec = provision::Spec {
        operator: operator.to_string(),
        helper: None,
        dry_run: true,
    };
    let (_accounts, fs) = provision::plan(&spec);
    let mut rows = vec![
        agent_user_row(view, operator, &agent, provisioned),
        agent_groups_row(view, operator, &agent, provisioned),
    ];
    // The fs artifact rows reuse the plan: one row per directory tree,
    // the helper and the share — each aggregating its actions.
    rows.push(artifact_row(
        "agent-home",
        view,
        fs.iter()
            .filter(|a| matches!(a, provision::Action::Dir { path, .. } if *path == AGENT_HOME)),
        provisioned,
    ));
    rows.push(artifact_row(
        "opt-tree",
        view,
        fs.iter().filter(
            |a| matches!(a, provision::Action::Dir { path, .. } if path.starts_with(OPT_ROOT)),
        ),
        provisioned,
    ));
    rows.push(artifact_row(
        "helper",
        view,
        fs.iter()
            .filter(|a| matches!(a, provision::Action::Install { .. })),
        provisioned,
    ));
    rows.push(artifact_row(
        "share-tree",
        view,
        fs.iter().filter(|a| {
            matches!(a, provision::Action::Dir { path, .. } if path.starts_with(VAR_LIB))
                || matches!(a, provision::Action::DefaultAcl { .. })
        }),
        provisioned,
    ));
    rows.push(home_acl_row(view, operator, &principals));
    rows.push(git_config_row(view, operator));
    Audit { rows, provisioned }
}

fn artifact_row<'a>(
    name: &'static str,
    view: &dyn View,
    actions: impl Iterator<Item = &'a provision::Action>,
    provisioned: bool,
) -> Row {
    let mut problems = Vec::new();
    let mut rendered = Vec::new();
    for action in actions {
        rendered.push(action.render());
        match action.assess(view) {
            Ok(provision::Assess::Clean) => {}
            Ok(provision::Assess::Blocked(why)) => problems.push(format!("refused: {why}")),
            Ok(provision::Assess::Needed(why)) | Ok(provision::Assess::Drift(why)) => {
                problems.push(why)
            }
            Err(e) => problems.push(format!("cannot assess: {e}")),
        }
    }
    let value = json!({"expect": rendered, "problems": problems});
    let hard = problems.iter().any(|p| p.starts_with("refused:"));
    let level = if problems.is_empty() {
        Level::Ok
    } else if hard || provisioned {
        Level::Fail
    } else {
        Level::Warn
    };
    let remedy = match level {
        Level::Ok => String::new(),
        Level::Warn => {
            "not provisioned — `cadence agent-uid provision` once, as root (ADR 0007 stage A)"
                .to_string()
        }
        Level::Fail => PROVISION_REMEDY.to_string(),
    };
    let detail = if problems.is_empty() {
        match name {
            "agent-home" => format!("{AGENT_HOME} is {AGENT_USER}:{AGENT_USER} 0750"),
            "opt-tree" => {
                "/opt/cadence tree: root root 0755; libexec 0750 cadence-launch; bin+releases 0755"
                    .to_string()
            }
            "helper" => format!("{HELPER_DEST} is root:{LAUNCH_GROUP} 4750"),
            "share-tree" => format!("{VAR_LIB}: lanes 0750, repos 2750 + default cadence rwX"),
            _ => "as provisioned".to_string(),
        }
    } else {
        problems.join("; ")
    };
    row(name, false, level, value, detail, remedy)
}

fn agent_user_row(view: &dyn View, operator: &str, agent: &Option<User>, provisioned: bool) -> Row {
    let name = "agent-user";
    let Some(u) = agent else {
        return row(
            name,
            false,
            if provisioned {
                Level::Fail
            } else {
                Level::Warn
            },
            json!({"present": false}),
            format!("no {AGENT_USER} account"),
            PROVISION_REMEDY.into(),
        );
    };
    let mut problems = Vec::new();
    if u.uid == 0 {
        problems.push("resolved to uid 0 — the account is root".to_string());
    }
    // A boundary between two accounts on the same uid is no
    // boundary — the agent would own the operator's seat.
    if let Ok(Some(op)) = view.user(operator) {
        if u.uid == op.uid {
            problems.push(format!(
                "resolved to uid {} — the operator's own uid",
                u.uid
            ));
        }
    }
    // A second account answering to the agent's uid is the same
    // collision under another name.
    if let Ok(users) = view.users() {
        for other in users {
            if other.uid == u.uid && other.name != AGENT_USER && other.name != operator {
                problems.push(format!(
                    "uid {} is shared with account {}",
                    u.uid, other.name
                ));
            }
        }
    }
    if u.shell != NOLOGIN {
        problems.push(format!("shell {} is not {NOLOGIN}", u.shell));
    }
    if u.home != AGENT_HOME {
        problems.push(format!("home {} is not {AGENT_HOME}", u.home));
    }
    let primary = u.gid.to_string();
    if let Ok(Some(gname)) = view.group_name(u.gid) {
        if gname != AGENT_USER {
            problems.push(format!("primary group {gname} is not {AGENT_USER}"));
        }
    }
    match u.locked {
        Some(false) => problems.push("password is not locked".to_string()),
        None => problems.push("password lock unverifiable — shadow needs root".to_string()),
        _ => {}
    }
    let mut detail = format!("{AGENT_USER} uid {} gid {primary}", u.uid);
    let level = if problems.is_empty() {
        Level::Ok
    } else {
        detail = format!("{detail}: {}", problems.join("; "));
        // An unverifiable lock alone is a warn, not a fail — the rest
        // are hard faults.
        if problems.iter().all(|p| p.contains("unverifiable")) {
            Level::Warn
        } else {
            Level::Fail
        }
    };
    row(
        name,
        false,
        level,
        json!({"uid": u.uid, "gid": u.gid, "home": u.home, "shell": u.shell, "locked": u.locked, "problems": problems}),
        detail,
        if level == Level::Ok {
            String::new()
        } else {
            PROVISION_REMEDY.into()
        },
    )
}

fn agent_groups_row(
    view: &dyn View,
    operator: &str,
    agent: &Option<User>,
    provisioned: bool,
) -> Row {
    let name = "agent-groups";
    let mut problems = Vec::new();
    let mut measured = json!({});
    let mut group_present = 0;
    for (gname, allowed) in [
        (AGENT_USER, vec![]),
        (
            SHARED_GROUP,
            vec![operator.to_string(), AGENT_USER.to_string()],
        ),
        (LAUNCH_GROUP, vec![operator.to_string()]),
    ] {
        match view.group(gname) {
            Ok(Some(g)) => {
                group_present += 1;
                let extra: Vec<&String> = g
                    .members
                    .iter()
                    .filter(|m| !allowed.iter().any(|a| a == *m))
                    .collect();
                if !extra.is_empty() {
                    problems.push(format!(
                        "{gname} has unexpected member(s): {}",
                        extra
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                // The shared edge only counts if both sides are in
                // it — and the launch edge must hold the operator, or
                // the setuid helper refuses the seat it was built
                // for. Membership is judged by the whole group vector
                // (supplementary and primary alike).
                if gname == SHARED_GROUP || gname == LAUNCH_GROUP {
                    for want in &allowed {
                        let member = view
                            .user(want)
                            .ok()
                            .flatten()
                            .and_then(|u| view.member_gids(&u).ok())
                            .is_some_and(|gids| gids.contains(&g.gid));
                        if !member {
                            problems.push(format!("{want} is not in {gname}"));
                        }
                    }
                }
                measured[gname] = json!({"gid": g.gid, "members": g.members});
            }
            Ok(None) => problems.push(format!("group {gname} is absent")),
            Err(e) => problems.push(format!("group {gname}: {e}")),
        }
    }
    // The agent must never sit in the operator's own primary group —
    // that is a traversal grant into `~` wearing a different name.
    if let Ok(Some(op)) = view.user(operator) {
        if let Ok(Some(gname)) = view.group_name(op.gid) {
            if let Ok(Some(g)) = view.group(&gname) {
                if g.members.contains(AGENT_USER) {
                    problems.push(format!(
                        "{AGENT_USER} is a member of the operator's own group {gname}"
                    ));
                }
            }
        }
    }
    let level = if problems.is_empty() {
        Level::Ok
    } else if !provisioned && group_present == 0 && agent.is_none() {
        Level::Warn
    } else {
        Level::Fail
    };
    let detail = if problems.is_empty() {
        format!("{AGENT_USER}, {SHARED_GROUP} (+{operator}, +{AGENT_USER}), {LAUNCH_GROUP} (+{operator}) — as §5")
    } else {
        problems.join("; ")
    };
    measured["problems"] = json!(problems);
    row(
        name,
        false,
        level,
        measured,
        detail,
        if level == Level::Ok {
            String::new()
        } else {
            PROVISION_REMEDY.into()
        },
    )
}

/// §4's home-side negative. `findings` are violations — an ACL or
/// mode that really reaches the agent domain. `unverified` is the
/// other half of honesty: the parts the sweep could not check (no ACL
/// support, a missing operator, a truncated walk). A violation fails;
/// an unverified piece warns — never a quiet pass.
pub struct Sweep {
    pub findings: Vec<String>,
    pub unverified: Vec<String>,
    /// The operator's home, resolved from the account — never `$HOME`
    /// (under sudo that is root's).
    pub home: Option<String>,
    pub scanned: usize,
}

pub fn home_acl_sweep(
    view: &dyn View,
    operator: &str,
    principals: &AgentPrincipals,
    budget: usize,
) -> Sweep {
    let mut sweep = Sweep {
        findings: Vec::new(),
        unverified: Vec::new(),
        home: None,
        scanned: 0,
    };
    if !view.acl_supported() {
        sweep
            .unverified
            .push("ACLs cannot be read on this platform".into());
        return sweep;
    }
    let home = view
        .user(operator)
        .ok()
        .flatten()
        .map(|u| u.home)
        .filter(|h| !h.is_empty());
    let Some(home) = home else {
        sweep
            .unverified
            .push(format!("operator account {operator} has no readable home"));
        return sweep;
    };
    sweep.home = Some(home.clone());
    // The home's own mode: `other` must be fully closed — traversal is
    // the grant the whole layout rests on.
    match view.stat(&home) {
        Ok(Some(m)) if m.mode & 0o007 != 0 => sweep.findings.push(format!(
            "{home} mode {:04o} grants `other` — world-traversable",
            m.mode
        )),
        Ok(None) => sweep.unverified.push(format!("{home} does not exist")),
        Err(e) => sweep.unverified.push(format!("{home}: cannot stat — {e}")),
        _ => {}
    }
    let check = |path: &str, sweep: &mut Sweep| match view.acls(path) {
        Ok(entries) => {
            for e in entries {
                if principals.grants_agent(&e) {
                    let who = match e.tag {
                        Principal::User(id) => format!("uid {id}"),
                        Principal::Group(id) => format!("gid {id}"),
                        _ => unreachable!(),
                    };
                    sweep.findings.push(format!(
                        "{path}: {}ACL grants {who} ({:03o})",
                        if e.default { "default " } else { "" },
                        e.perms
                    ));
                }
            }
        }
        Err(e) => sweep
            .unverified
            .push(format!("{path}: cannot read ACLs — {e}")),
    };
    // Mode and ownership grants the ACL pass cannot see: an
    // agent-owned path, a chgrp into an agent group with group bits
    // on, or a world-writable path each reach the agent domain with
    // no ACL attached.
    let check_mode = |path: &str, sweep: &mut Sweep| match view.stat(path) {
        Ok(Some(m)) => {
            if m.is_symlink {
                return;
            }
            if let Some(auid) = principals.uid {
                if m.uid == auid {
                    sweep
                        .findings
                        .push(format!("{path}: owned by the agent uid {auid}"));
                }
            }
            if principals.gids.contains(&m.gid) && m.mode & 0o070 != 0 {
                sweep.findings.push(format!(
                    "{path}: group-owned by an agent group gid {} with {:04o} — a chgrp grant",
                    m.gid, m.mode
                ));
            }
            if m.mode & 0o002 != 0 {
                sweep
                    .findings
                    .push(format!("{path}: mode {:04o} is world-writable", m.mode));
            }
        }
        Ok(None) => {}
        Err(e) => sweep.unverified.push(format!("{path}: cannot stat — {e}")),
    };
    check(&home, &mut sweep);
    check_mode(&home, &mut sweep);
    match view.walk(&home, budget) {
        Ok((paths, truncated)) => {
            sweep.scanned = paths.len();
            if truncated {
                sweep.unverified.push(format!(
                    "walk truncated at {budget} entries — the tail is unchecked"
                ));
            }
            for path in &paths {
                check(path, &mut sweep);
                check_mode(path, &mut sweep);
            }
        }
        Err(e) => sweep.unverified.push(format!("cannot walk {home}: {e}")),
    }
    sweep
}

/// The pre-flight shape of the sweep: provision refuses on a violation
/// *and* on an unverifiable piece — "can't prove it" is not a pass
/// when the arming is about to happen.
pub fn home_acl_findings(
    view: &dyn View,
    operator: &str,
    principals: &AgentPrincipals,
    budget: usize,
) -> Vec<String> {
    let sweep = home_acl_sweep(view, operator, principals, budget);
    let mut out = sweep.findings;
    out.extend(
        sweep
            .unverified
            .into_iter()
            .map(|u| format!("unverified: {u}")),
    );
    out
}

fn home_acl_row(view: &dyn View, operator: &str, principals: &AgentPrincipals) -> Row {
    let sweep = home_acl_sweep(view, operator, principals, HOME_ACL_WALK_BUDGET);
    let level = if !sweep.findings.is_empty() {
        Level::Fail
    } else if !sweep.unverified.is_empty() {
        Level::Warn
    } else {
        Level::Ok
    };
    let detail = if !sweep.findings.is_empty() {
        format!(
            "agent-reachable entries under the operator's home: {}",
            sweep.findings.len()
        )
    } else if !sweep.unverified.is_empty() {
        format!(
            "no agent-reachable ACL in the {} paths checked under {} — but {}",
            sweep.scanned,
            sweep.home.as_deref().unwrap_or("~"),
            sweep.unverified.join("; ")
        )
    } else {
        format!(
            "no ACL under {} grants the agent domain ({} paths checked)",
            sweep.home.as_deref().unwrap_or("~"),
            sweep.scanned
        )
    };
    let remedy = if sweep.findings.is_empty() {
        String::new()
    } else {
        let mut s = String::new();
        for f in &sweep.findings {
            let _ = writeln!(s, "  {f}");
        }
        let _ = write!(
            s,
            "remove them — `setfacl -b <path>` strips an ACL; §4 rule 2: no ACL under ~ may reach \
             the agent uid or its groups"
        );
        s
    };
    row(
        "home-acl",
        true,
        level,
        json!({
            "home": sweep.home,
            "scanned": sweep.scanned,
            "acl_supported": view.acl_supported(),
            "findings": sweep.findings,
            "unverified": sweep.unverified,
        }),
        detail,
        remedy,
    )
}

// ---------- the git-config negative ----------

/// One parsed gitconfig line: `section` is lowercased and includes the
/// subsection (`includeif "x"` → `includeif.x`), `key` lowercased.
#[derive(Debug)]
struct CfgLine {
    section: String,
    key: String,
    value: String,
}

/// A minimal gitconfig parser — enough for `safe.directory` and
/// `include*` hunting. Handles `#`/`;` comments, `[s "sub"]`
/// subsections, `key` (implicit true), `key = value`, quoted values,
/// and line continuations inside quotes.
fn parse_gitconfig(text: &str) -> Vec<CfgLine> {
    let mut out = Vec::new();
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(end) = rest.find(']') {
                let inner = &rest[..end];
                section = if let Some(q0) = inner.find('"') {
                    let base = inner[..q0].trim().to_lowercase();
                    match inner.rfind('"') {
                        Some(q1) if q1 > q0 => {
                            format!("{base}.{}", &inner[q0 + 1..q1])
                        }
                        _ => base,
                    }
                } else {
                    inner.trim().to_lowercase()
                };
            }
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), unquote(v.trim())),
            None => (line, "true".to_string()),
        };
        out.push(CfgLine {
            section: section.clone(),
            key: key.trim().to_lowercase(),
            value,
        });
    }
    out
}

/// Strip one layer of double quotes; honour `\\` and `\"`; a trailing
/// `#`/`;` outside quotes starts a comment.
fn unquote(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '#' | ';' if !quoted => break,
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Does a `safe.directory` value cover `store`? git's grammar has
/// three armed forms, and all three are flagged:
///
/// - `*` alone — every path on the host.
/// - `dir/*` — a trailing `/*` covers everything *under* `dir`
///   recursively, so `/var/*`, `/*` and `/var/lib/cadence/*` all arm
///   the store's checkouts even though none equals it.
/// - a literal path — flags the store itself or a descendant (an
///   armed exception for an agent-owned checkout). Ancestor paths do
///   NOT cover under git semantics and are not flagged.
fn covers_store(value: &str, store: &str, home: &str) -> bool {
    let value = value.trim();
    if value == "*" {
        return true;
    }
    let expanded = if let Some(rest) = value.strip_prefix("~/") {
        format!("{}/{}", home.trim_end_matches('/'), rest)
    } else if value.contains("%(prefix)") {
        // `%(prefix)` expands to git's own install prefix at runtime —
        // cannot be decided here; treat as suspicious.
        return true;
    } else {
        value.to_string()
    };
    // Lexical normalization: `.`, `..`, repeated slashes.
    let mut norm: Vec<&str> = Vec::new();
    for part in expanded.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                norm.pop();
            }
            p => norm.push(p),
        }
    }
    let store = Path::new(store);
    // `dir/*` covers every path under dir: the store must not sit
    // beneath the wildcard's root.
    if norm.last() == Some(&"*") {
        norm.pop();
        let dir = PathBuf::from(format!("/{}", norm.join("/")));
        return store.starts_with(&dir);
    }
    let norm = PathBuf::from(format!("/{}", norm.join("/")));
    norm == store || norm.starts_with(store)
}

/// Config keys that make git *run something*: a value reaching the
/// agent domain is a command execution channel, and a config file
/// already inside it sets them for free. This is the negative's
/// second edge — `safe.directory` is not the only way a checkout
/// talks back.
fn exec_capable_key(section: &str, key: &str) -> bool {
    let full = format!("{section}.{key}");
    matches!(
        full.as_str(),
        "core.fsmonitor"
            | "core.hookspath"
            | "core.pager"
            | "core.editor"
            | "core.sshcommand"
            | "core.askpass"
            | "core.gitproxy"
            | "diff.external"
            | "credential.helper"
            | "sequence.editor"
            | "interactive.difffilter"
    ) || (section == "credential" && key == "helper")
        || (section.starts_with("credential.") && key == "helper")
        || (section.starts_with("filter.") && matches!(key, "clean" | "smudge" | "process"))
        || (section.starts_with("merge.") && key == "driver")
        || (section.starts_with("diff.") && matches!(key, "textconv" | "command"))
        || (section == "gpg" && key == "program")
        || (section.starts_with("gpg.") && key == "program")
        // `*.cmd`-carrying tool sections: difftool, mergetool,
        // browser, guitool — each names a shell command.
        || (key == "cmd"
            && (section.starts_with("difftool.")
                || section.starts_with("mergetool.")
                || section.starts_with("browser.")
                || section.starts_with("guitool.")))
        || section == "pager"
        || section.starts_with("pager.")
}

/// The findings shape both the `git-config` row and provision's
/// pre-flight use.
pub fn git_config_findings(view: &dyn View, operator: &str) -> Vec<String> {
    let home = view
        .user(operator)
        .ok()
        .flatten()
        .map(|u| u.home)
        .unwrap_or_default();
    let mut findings = Vec::new();
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();

    // The protected-config surface for uid-1000 git: system file (unless
    // GIT_CONFIG_NOSYSTEM), the global file(s), and env-carried pairs.
    let mut files: Vec<PathBuf> = Vec::new();
    if view.env("GIT_CONFIG_NOSYSTEM").is_none() {
        let path = view
            .env("GIT_CONFIG_SYSTEM")
            .unwrap_or_else(|| "/etc/gitconfig".to_string());
        if reaches_store(&path, VAR_LIB, &home, Path::new("/")) {
            findings.push(format!(
                "GIT_CONFIG_SYSTEM={path} reads config from the agent domain"
            ));
        }
        files.push(PathBuf::from(path));
    }
    match view.env("GIT_CONFIG_GLOBAL") {
        Some(path) => {
            if reaches_store(&path, VAR_LIB, &home, Path::new("/")) {
                findings.push(format!(
                    "GIT_CONFIG_GLOBAL={path} reads config from the agent domain"
                ));
            }
            files.push(PathBuf::from(path));
        }
        None => {
            // With no operator account there is no uid-1000 git config
            // to violate — the negative holds vacuously.
            if !home.is_empty() {
                files.push(PathBuf::from(format!("{home}/.gitconfig")));
                files.push(PathBuf::from(format!("{home}/.config/git/config")));
            }
        }
    }
    for file in files.clone() {
        scan_git_file(view, &file, &home, &mut visited, &mut findings);
    }

    // Environment-carried config: GIT_CONFIG_COUNT pairs and the
    // serialized GIT_CONFIG_PARAMETERS are protected config too.
    if let Some(count) = view
        .env("GIT_CONFIG_COUNT")
        .and_then(|c| c.parse::<usize>().ok())
    {
        for i in 0..count.min(64) {
            let key = view
                .env(&format!("GIT_CONFIG_KEY_{i}"))
                .unwrap_or_default()
                .to_lowercase();
            let val = view
                .env(&format!("GIT_CONFIG_VALUE_{i}"))
                .unwrap_or_default();
            if key == "safe.directory" && covers_store(&val, VAR_LIB, &home) {
                findings.push(format!("env GIT_CONFIG_KEY_{i}: safe.directory={val}"));
            }
            if key == "include.path" && reaches_store(&val, VAR_LIB, &home, Path::new("/")) {
                findings.push(format!("env GIT_CONFIG_KEY_{i}: include.path={val}"));
            }
            if exec_capable_full(&key) && reaches_store(&val, VAR_LIB, &home, Path::new("/")) {
                findings.push(format!(
                    "env GIT_CONFIG_KEY_{i}: exec-capable {key}={val} reaches into {VAR_LIB}"
                ));
            }
        }
    }
    if let Some(params) = view.env("GIT_CONFIG_PARAMETERS") {
        for (key, val) in parse_config_parameters(&params) {
            if key == "safe.directory" && covers_store(&val, VAR_LIB, &home) {
                findings.push(format!("GIT_CONFIG_PARAMETERS: safe.directory={val}"));
            }
            if key == "include.path" && reaches_store(&val, VAR_LIB, &home, Path::new("/")) {
                findings.push(format!("GIT_CONFIG_PARAMETERS: include.path={val}"));
            }
            if exec_capable_full(&key) && reaches_store(&val, VAR_LIB, &home, Path::new("/")) {
                findings.push(format!(
                    "GIT_CONFIG_PARAMETERS: exec-capable {key}={val} reaches into {VAR_LIB}"
                ));
            }
        }
    }
    findings
}

/// The env spelling of an exec-capable key: `section.key` with the
/// subsection folded into the section (the parser already produces
/// `credential.https://x` as the section).
fn exec_capable_full(full_key: &str) -> bool {
    match full_key.rsplit_once('.') {
        Some((section, key)) => exec_capable_key(section, key),
        None => false,
    }
}

/// `include.path` that reaches *into* the store is its own violation —
/// the included file is agent-written config in the operator's git.
/// Unlike `safe.directory` there is no `*` form; only a path equal to
/// or under the store counts.
fn reaches_store(value: &str, store: &str, home: &str, base: &Path) -> bool {
    let value = value.trim();
    let target = if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(format!("{}/{}", home.trim_end_matches('/'), rest))
    } else {
        let t = PathBuf::from(value);
        if t.is_absolute() {
            t
        } else {
            base.join(t)
        }
    };
    let mut norm: Vec<String> = Vec::new();
    for c in target.components() {
        use std::path::Component;
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                norm.pop();
            }
            Component::Normal(p) => norm.push(p.display().to_string()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let norm = PathBuf::from(format!("/{}", norm.join("/")));
    let store = Path::new(store);
    norm == store || norm.starts_with(store)
}

/// Squote-tokenize `'key'='value'` pairs out of GIT_CONFIG_PARAMETERS.
fn parse_config_parameters(params: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = params.chars().peekable();
    loop {
        // skip spaces
        while chars.peek() == Some(&' ') {
            chars.next();
        }
        if chars.next() != Some('\'') {
            break;
        }
        let mut key = String::new();
        for c in chars.by_ref() {
            if c == '\'' {
                break;
            }
            key.push(c);
        }
        // expect ='
        if chars.next() != Some('=') || chars.next() != Some('\'') {
            break;
        }
        let mut val = String::new();
        while let Some(c) = chars.next() {
            if c == '\'' {
                break;
            }
            if c == '\\' {
                if let Some(n) = chars.next() {
                    val.push(n);
                    continue;
                }
                break;
            }
            val.push(c);
        }
        out.push((key.to_lowercase(), val));
    }
    out
}

fn scan_git_file(
    view: &dyn View,
    path: &Path,
    home: &str,
    visited: &mut BTreeSet<PathBuf>,
    findings: &mut Vec<String>,
) {
    if visited.len() >= 64 || !visited.insert(path.to_path_buf()) {
        return;
    }
    let Ok(bytes) = view.read_file(&path.display().to_string()) else {
        // A path we refused to open may still matter: git follows a
        // symlinked include — if the link lands inside the store the
        // chain reads agent-written config. Resolve it (link text
        // only) and flag the landing, never the bytes.
        if let Ok(Some(m)) = view.stat(&path.display().to_string()) {
            if m.is_symlink {
                if let Ok(t) = view.read_link(&path.display().to_string()) {
                    let base = path.parent().unwrap_or(Path::new("/")).to_path_buf();
                    let t = if t.is_absolute() { t } else { base.join(t) };
                    if reaches_store(&t.display().to_string(), VAR_LIB, home, Path::new("/")) {
                        findings.push(format!(
                            "{}: symlinked config resolves into {VAR_LIB} — git would follow it",
                            path.display()
                        ));
                    }
                }
            }
        }
        return; // absent config is inert
    };
    let text = String::from_utf8_lossy(&bytes);
    // A config file sitting inside the store is agent-written — every
    // exec-capable key in it is a command channel regardless of the
    // value it carries.
    let file_in_store = reaches_store(&path.display().to_string(), VAR_LIB, home, Path::new("/"));
    for line in parse_gitconfig(&text) {
        let full_key = format!("{}.{}", line.section, line.key);
        if full_key == "safe.directory" && covers_store(&line.value, VAR_LIB, home) {
            findings.push(format!(
                "{}: safe.directory={} covers {VAR_LIB}",
                path.display(),
                line.value
            ));
        }
        if exec_capable_key(&line.section, &line.key) {
            if file_in_store {
                findings.push(format!(
                    "{}: exec-capable {}={} set by config inside the agent domain",
                    path.display(),
                    full_key,
                    line.value
                ));
            } else if reaches_store(&line.value, VAR_LIB, home, Path::new("/")) {
                findings.push(format!(
                    "{}: exec-capable {}={} reaches into {VAR_LIB}",
                    path.display(),
                    full_key,
                    line.value
                ));
            }
        }
        let is_include = full_key == "include.path"
            || (line.section.starts_with("includeif") && line.key == "path");
        if is_include {
            let base = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            let target = if let Some(rest) = line.value.strip_prefix("~/") {
                PathBuf::from(format!("{}/{}", home.trim_end_matches('/'), rest))
            } else {
                let t = PathBuf::from(&line.value);
                if t.is_absolute() {
                    t
                } else {
                    base.join(t)
                }
            };
            if reaches_store(&line.value, VAR_LIB, home, &base) {
                findings.push(format!(
                    "{}: include.path={} reaches into {VAR_LIB}",
                    path.display(),
                    line.value
                ));
            } else {
                scan_git_file(view, &target, home, visited, findings);
            }
        }
    }
}

fn git_config_row(view: &dyn View, operator: &str) -> Row {
    let findings = git_config_findings(view, operator);
    let level = if findings.is_empty() {
        Level::Ok
    } else {
        Level::Fail
    };
    let detail = if findings.is_empty() {
        format!("no uid-1000 git config covers {VAR_LIB}")
    } else {
        format!("{} violating config entrie(s):", findings.len())
    };
    let remedy = if findings.is_empty() {
        String::new()
    } else {
        let mut s = String::new();
        for f in &findings {
            let _ = writeln!(s, "  {f}");
        }
        let _ = write!(
            s,
            "remove the lines — §4 rule 2: one `safe.directory`/`include.path` covering \
             {VAR_LIB} re-opens the agent→operator code-exec channel"
        );
        s
    };
    row(
        "git-config",
        true,
        level,
        json!({"findings": findings}),
        detail,
        remedy,
    )
}

/// `cadence agent-uid doctor` — print every row, exit on the worst.
pub fn cli(json_out: bool, operator: &str) -> Result<i32> {
    let view = LiveHost::new();
    let report = audit(&view, operator);
    if json_out {
        let checks: Vec<Value> = report
            .rows
            .iter()
            .map(|r| {
                json!({
                    "name": r.name,
                    "level": r.level.as_str(),
                    "negative": r.negative,
                    "value": r.value,
                    "detail": r.detail,
                    "remedy": r.remedy,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "level": report.level().as_str(),
                "provisioned": report.provisioned,
                "checks": checks,
            }))
            .unwrap_or_default()
        );
    } else {
        for r in &report.rows {
            println!("{:5} {:<12} {}", r.level.as_str(), r.name, r.detail);
            if !r.remedy.is_empty() {
                println!("       ↳ {}", r.remedy.replace('\n', "\n       ↳ "));
            }
        }
        println!("level: {}", report.level().as_str());
    }
    Ok(match report.level() {
        Level::Fail => 2,
        Level::Warn => 1,
        Level::Ok => 0,
    })
}

/// The `doctor --host` summary row: the whole audit folded into one
/// verdict for the standing watchdog.
/// `(level-str, detail, remedy, value)`.
pub fn host_summary(view: &dyn View, operator: &str) -> (&'static str, String, String, Value) {
    let report = audit(view, operator);
    let violations: Vec<&str> = report.violations().iter().map(|r| r.name).collect();
    if !report.provisioned {
        if violations.is_empty() {
            return (
                "ok",
                "agent-uid separation is not provisioned on this host (ADR 0007 stage A \
                 has not run)"
                    .to_string(),
                String::new(),
                json!({"provisioned": false}),
            );
        }
        return (
            "warn",
            format!(
                "not provisioned, but a boundary violation is already armed: {}",
                violations.join(", ")
            ),
            "clear it before provisioning — `cadence agent-uid doctor` lists it".to_string(),
            json!({"provisioned": false, "violations": violations}),
        );
    }
    let failing = report.failing_names();
    match report.level() {
        Level::Ok => (
            "ok",
            "agent-uid boundary provisioned; every artifact and both negative assertions \
             hold"
                .to_string(),
            String::new(),
            json!({"provisioned": true, "rows": report.rows.len()}),
        ),
        level => (
            level.as_str(),
            format!("agent-uid boundary: failing {}", failing.join(", ")),
            "audit in full: `cadence agent-uid doctor`".to_string(),
            json!({"provisioned": true, "failing": failing, "violations": violations}),
        ),
    }
}
