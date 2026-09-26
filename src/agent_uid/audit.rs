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
    provision, AgentPrincipals, LiveHost, Meta, Principal, User, View, AGENT_HOME, AGENT_USER,
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
    rows.push(git_config_row(view, operator, &principals));
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
    // Every supplementary membership is a grant — `usermod -aG docker
    // cadence-agent` hands the agent the docker socket's root-equivalent
    // reach. The allowlist is the §5 set: the agent's own primary gid
    // and the shared group — nothing else.
    if let Some(u) = agent {
        if let Ok(gids) = view.member_gids(u) {
            let shared = view.group(SHARED_GROUP).ok().flatten().map(|g| g.gid);
            for gid in gids {
                if gid == u.gid || Some(gid) == shared {
                    continue;
                }
                let name = view
                    .group_name(gid)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("gid {gid}"));
                problems.push(format!(
                    "{AGENT_USER} is a member of {name} — outside the §5 group set"
                ));
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

/// What the sweep found. `findings` are violations — config that arms
/// the agent→operator channel. `unverified` is the other half of the
/// negative: every piece the sweep could not check (an unreadable or
/// over-cap file, a link chain that never lands, a git parse error).
/// A negative assertion cannot claim a pass it never checked, so an
/// unverifiable piece fails closed — never `ok`.
#[derive(Default)]
pub struct CfgAudit {
    pub findings: Vec<String>,
    pub unverified: Vec<String>,
}

/// How a config-spelled path resolves once `~`, `~user`, relative
/// forms and every symlink hop are worked through.
enum Resolve {
    /// Reached a terminal path.
    Done(Resolved),
    /// Couldn't decide — the chain is a finding of the unverifiable
    /// kind.
    Unverifiable(String),
}

struct Resolved {
    /// The terminal logical path, all link hops consumed.
    path: PathBuf,
    /// The landing is agent-writable: under a static root (the store
    /// or the agent's home) or live evidence on some component —
    /// agent-owned, agent-group-writable, world-writable, or an ACL
    /// write grant. For a missing tail this reflects the nearest
    /// existing ancestor.
    in_domain: bool,
    /// What the terminal lstat said.
    terminal: Term,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Term {
    /// Nothing there — an inert include; an exec value whose parent
    /// decides reach.
    Missing,
    /// Exists but is not a regular file (fifo, dir, …) — git would
    /// hang or refuse; the audit fails closed.
    NotFile,
    File,
}

/// The agent's static writable roots: the store (`repos` is
/// agent-owned by spec) and the agent's own home.
fn static_domain(p: &Path) -> bool {
    p.starts_with(VAR_LIB) || p.starts_with(AGENT_HOME)
}

/// Lexical path normalization — `.`/`..`/repeated slashes — with no
/// filesystem access.
fn normalize_path(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in p.components() {
        use std::path::Component;
        match c {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
        }
    }
    out
}

/// Expand a config-spelled path to a normalized logical absolute path:
/// `~/x` → the operator's home, `~user/x` → that user's home from the
/// passwd map (an unresolvable user stays a literal `~user` component,
/// the way git treats it), relative → `base`.
fn expand_cfg_path(view: &dyn View, raw: &str, home: &str, base: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return normalize_path(Path::new(home));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return normalize_path(&Path::new(home).join(rest));
    }
    if let Some(rest) = raw.strip_prefix('~') {
        let (name, tail) = match rest.split_once('/') {
            Some((n, t)) => (n, t),
            None => (rest, ""), // a bare `~user` is that user's home
        };
        if let Ok(Some(u)) = view.user(name) {
            if !u.home.is_empty() {
                return normalize_path(&Path::new(&u.home).join(tail));
            }
        }
    }
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        normalize_path(&p)
    } else {
        normalize_path(&base.join(p))
    }
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
fn covers_store(view: &dyn View, value: &str, store: &str, home: &str) -> bool {
    let value = value.trim();
    if value == "*" {
        return true;
    }
    if value.contains("%(prefix)") {
        // `%(prefix)` expands to git's own install prefix at runtime —
        // cannot be decided here; treat as suspicious.
        return true;
    }
    let p = expand_cfg_path(view, value, home, Path::new("/"));
    let store = Path::new(store);
    // `dir/*` covers every path under dir: the store must not sit
    // beneath the wildcard's root.
    if p.file_name().is_some_and(|n| n == "*") {
        return store.starts_with(p.parent().unwrap_or(Path::new("/")));
    }
    p == store || p.starts_with(store)
}

/// How strongly a statted component puts its subtree in the agent's
/// reach.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No agent write vector found.
    No,
    /// The agent can create children but cannot unlink existing ones —
    /// a write grant on a *sticky* dir (`/tmp` is `0o1777` by design).
    /// It counts only if the child it guards turns out to be missing.
    Pending,
    /// The agent writes this node or its children outright.
    Yes,
}

/// Live evidence that a statted component is agent-writable:
/// agent-owned, group-writable to an agent group, world-writable, or
/// an ACL entry that grants the agent principals write. Directories
/// need `w+x` (a writable-but-unenterable dir grants nothing); files
/// need `w`. A symlink's own mode is always `0o777` and grants nothing —
/// the link's fate is decided by the parent dir's grant. On a sticky
/// dir, a non-owner grant (group/other/ACL) protects the dir's existing
/// children: the agent can plant *new* names but not replace them, so
/// the reach is deferred (`Pending`) until a missing tail proves it.
fn writable_grant(view: &dyn View, path: &str, m: &Meta, principals: &AgentPrincipals) -> Grant {
    if m.is_symlink {
        return Grant::No;
    }
    if principals.uid.is_some_and(|u| u == m.uid) {
        // Owner can unlink in its own dir — sticky never bites.
        return Grant::Yes;
    }
    let (g_bits, o_bits, need) = if m.is_dir {
        (0o030, 0o003, 0b011)
    } else {
        (0o020, 0o002, 0b010)
    };
    let granted = (principals.gids.contains(&m.gid) && m.mode & g_bits == g_bits)
        || m.mode & o_bits == o_bits
        || view
            .acls(path)
            .map(|entries| {
                let mask = entries
                    .iter()
                    .find(|e| !e.default && e.tag == Principal::Mask)
                    .map(|e| e.perms)
                    .unwrap_or(0b111);
                entries.iter().any(|e| {
                    !e.default && principals.grants_agent(e) && (e.perms & mask) & need == need
                })
            })
            .unwrap_or(false);
    if !granted {
        return Grant::No;
    }
    if m.is_dir && m.mode & 0o1000 != 0 {
        Grant::Pending
    } else {
        Grant::Yes
    }
}

/// Resolve a config path value to its terminal, following every
/// symlink hop — in the final component *and* in every intermediate,
/// since git follows all of them at use time. Each hop is checked
/// against the agent domain before descending: a link that lands in
/// agent-writable ground means agent-written config. Bounded at 16
/// hops; a cycle or an overrun is unverifiable.
fn resolve_cfg_path(
    view: &dyn View,
    raw: &str,
    home: &str,
    base: &Path,
    principals: &AgentPrincipals,
) -> Resolve {
    let mut path = expand_cfg_path(view, raw, home, base);
    let mut in_domain = static_domain(&path);
    let mut seen = BTreeSet::new();
    let mut hops = 0;
    'outer: loop {
        if hops > 16 {
            return Resolve::Unverifiable(format!("{raw}: link chain exceeds 16 hops"));
        }
        let comps: Vec<std::ffi::OsString> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n.to_os_string()),
                _ => None,
            })
            .collect();
        let mut cur = PathBuf::from("/");
        // A sticky-dir grant on the last-walked dir: it reaches a
        // missing child but protects the ones that exist.
        let mut pending = false;
        for (i, c) in comps.iter().enumerate() {
            cur.push(c);
            let logical = cur.display().to_string();
            let m = match view.stat(&logical) {
                Ok(Some(m)) => m,
                Ok(None) => {
                    // The tail cannot exist — reach is decided by the
                    // ancestors already walked, and a sticky-granted
                    // parent means the agent can plant this very name.
                    return Resolve::Done(Resolved {
                        path,
                        in_domain: in_domain || pending,
                        terminal: Term::Missing,
                    });
                }
                Err(e) => {
                    return Resolve::Unverifiable(format!("{logical}: cannot stat — {e}"));
                }
            };
            // The component exists — a parent's sticky grant cannot
            // touch it, so the deferral lapses before this one judges.
            pending = false;
            if m.is_symlink {
                if !seen.insert(cur.clone()) {
                    return Resolve::Unverifiable(format!("{raw}: link cycle at {logical}"));
                }
                hops += 1;
                let t = match view.read_link(&logical) {
                    Ok(t) => t,
                    Err(e) => {
                        return Resolve::Unverifiable(format!("{logical}: cannot read link — {e}"))
                    }
                };
                let mut next = if t.is_absolute() {
                    normalize_path(&t)
                } else {
                    normalize_path(&cur.parent().unwrap_or(Path::new("/")).join(&t))
                };
                for rest in &comps[i + 1..] {
                    next.push(rest);
                }
                path = next;
                if static_domain(&path) {
                    in_domain = true;
                }
                continue 'outer;
            }
            if !in_domain {
                match writable_grant(view, &logical, &m, principals) {
                    Grant::Yes => in_domain = true,
                    Grant::Pending => pending = true,
                    Grant::No => {}
                }
            }
        }
        let terminal = if comps.is_empty() {
            // "/" itself — not a file.
            Term::NotFile
        } else {
            let logical = path.display().to_string();
            match view.stat(&logical) {
                Ok(Some(m)) if m.is_file => Term::File,
                Ok(Some(_)) => Term::NotFile,
                Ok(None) => Term::Missing,
                Err(e) => return Resolve::Unverifiable(format!("{logical}: cannot stat — {e}")),
            }
        };
        return Resolve::Done(Resolved {
            path,
            in_domain,
            terminal,
        });
    }
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
            | "sendemail.sendmailcmd"
            | "man.viewer"
            | "web.browser"
            | "ssh.variant"
            | "protocol.allow"
    ) || (section == "credential" && key == "helper")
        || (section.starts_with("credential.") && key == "helper")
        || (section.starts_with("filter.") && matches!(key, "clean" | "smudge" | "process"))
        || (section.starts_with("merge.") && key == "driver")
        || (section.starts_with("diff.") && matches!(key, "textconv" | "command"))
        || (section == "gpg" && key == "program")
        || (section.starts_with("gpg.") && key == "program")
        || (section.starts_with("sendemail.") && key == "sendmailcmd")
        // `*.cmd`-carrying tool sections: difftool, mergetool,
        // browser, guitool, man — each names a shell command; their
        // `*.path` siblings name the binary git executes.
        || (matches!(key, "cmd" | "path")
            && (section.starts_with("difftool.")
                || section.starts_with("mergetool.")
                || section.starts_with("browser.")
                || section.starts_with("guitool.")
                || section.starts_with("man.")))
        // `instaweb.*` names the httpd and friends it launches.
        || section == "instaweb"
        || section.starts_with("instaweb.")
        // `protocol.<scheme>.allow` re-arms `ext::`/`file` transports —
        // an armed transport runs the scheme's helper.
        || (section.starts_with("protocol.") && key == "allow")
        || section == "pager"
        || section.starts_with("pager.")
}

/// A `!`-alias is a shell command line, not a path — `git co` runs it
/// through `sh -c` as the operator. Path analysis cannot bound what a
/// shell payload reaches, so it flags wherever it is set.
fn is_exec_alias(section: &str, value: &str) -> bool {
    section == "alias" && value.trim_start().starts_with('!')
}

/// Does a config *value* reach the agent domain? Exec-capable values
/// are path-or-command strings — every path-looking token (split on
/// shell separators, `VAR=` prefixes unwrapped) is resolved the same
/// way an include target is: `ssh -i /var/lib/cadence/k` reaches even
/// though the whole value does not parse as one path.
fn value_reaches_domain(
    view: &dyn View,
    value: &str,
    home: &str,
    principals: &AgentPrincipals,
) -> bool {
    for tok in value.split(|c: char| c.is_whitespace() || ";|&(){}<>`'\"".contains(c)) {
        let tok = tok.rsplit_once('=').map(|(_, v)| v).unwrap_or(tok);
        let tok = tok.trim();
        if !(tok.starts_with('/') || tok.starts_with('~') || tok.starts_with('.')) {
            continue;
        }
        if let Resolve::Done(r) = resolve_cfg_path(view, tok, home, Path::new("/"), principals) {
            if r.in_domain {
                return true;
            }
        }
    }
    false
}

/// The read-only side of the sweep: the view plus the operator home
/// (`~` expansion) and the agent principals (writable-ground reach).
struct Ctx<'a> {
    view: &'a dyn View,
    home: &'a str,
    principals: &'a AgentPrincipals,
}

/// One parsed `section.key = value` and where it came from — a file
/// (`dir`/`in_domain` of its resolved landing) or an env source label.
struct Entry<'a> {
    origin: &'a str,
    dir: &'a Path,
    in_domain: bool,
    key: &'a str,
    value: &'a str,
}

/// Evaluate one parsed `(section.key, value)` pair from `origin`
/// (a file path or an env source label). `in_domain` means the
/// config bytes are already agent-written — every exec-capable key
/// flags regardless of the value it carries.
fn eval_cfg_entry(ctx: &Ctx, entry: Entry, visited: &mut BTreeSet<PathBuf>, rep: &mut CfgAudit) {
    let view = ctx.view;
    let home = ctx.home;
    let principals = ctx.principals;
    let (origin, file_dir, file_in_domain, full_key, value) = (
        entry.origin,
        entry.dir,
        entry.in_domain,
        entry.key,
        entry.value,
    );
    let (section, key) = full_key.rsplit_once('.').unwrap_or(("", full_key));
    if full_key == "safe.directory" {
        if covers_store(view, value, VAR_LIB, home) {
            rep.findings
                .push(format!("{origin}: safe.directory={value} covers {VAR_LIB}"));
        } else {
            // The store is not the only armed ground: a trust entry
            // rooted on anything the agent can write (its home, an
            // agent-writable dir, a sticky dir it can plant names
            // under) turns that checkout into the exec channel.
            let root = {
                let v = value.trim().trim_end_matches('*').trim_end_matches('/');
                if v.is_empty() {
                    "/"
                } else {
                    v
                }
            };
            match resolve_cfg_path(view, root, home, file_dir, principals) {
                Resolve::Unverifiable(why) => rep
                    .unverified
                    .push(format!("{origin}: safe.directory={value}: {why}")),
                Resolve::Done(r) => {
                    let dir_grant = r.terminal != Term::Missing && {
                        let logical = r.path.display().to_string();
                        view.stat(&logical).ok().flatten().is_some_and(|m| {
                            m.is_dir && writable_grant(view, &logical, &m, principals) != Grant::No
                        })
                    };
                    if r.terminal != Term::File && (r.in_domain || dir_grant) {
                        rep.findings.push(format!(
                            "{origin}: safe.directory={value} arms a checkout on \
                             agent-writable ground ({})",
                            r.path.display()
                        ));
                    }
                }
            }
        }
    }
    let is_include =
        full_key == "include.path" || (section.starts_with("includeif") && key == "path");
    if is_include {
        // Git honors include.path even from env-carried config — the
        // target joins the walk like any other file.
        match resolve_cfg_path(view, value, home, file_dir, principals) {
            Resolve::Unverifiable(why) => rep
                .unverified
                .push(format!("{origin}: include {value}: {why}")),
            Resolve::Done(r) if r.in_domain => rep.findings.push(format!(
                "{origin}: include.path={value} reads config from the agent domain"
            )),
            Resolve::Done(r) => scan_git_file(ctx, &r.path, visited, rep),
        }
        return;
    }
    if is_exec_alias(section, value) {
        rep.findings.push(format!(
            "{origin}: alias {full_key}={value} runs a shell payload as the operator — \
             `!` commands cannot be path-bounded"
        ));
        return;
    }
    if exec_capable_key(section, key) {
        if file_in_domain {
            rep.findings.push(format!(
                "{origin}: exec-capable {full_key}={value} set by config inside the agent domain"
            ));
        } else if value_reaches_domain(view, value, home, principals) {
            rep.findings.push(format!(
                "{origin}: exec-capable {full_key}={value} reaches the agent domain"
            ));
        }
    }
}

/// The findings shape both the `git-config` row and provision's
/// pre-flight use.
pub fn git_config_audit(view: &dyn View, operator: &str, principals: &AgentPrincipals) -> CfgAudit {
    let home = view
        .user(operator)
        .ok()
        .flatten()
        .map(|u| u.home)
        .unwrap_or_default();
    let mut rep = CfgAudit::default();
    let mut visited: BTreeSet<PathBuf> = BTreeSet::new();
    let ctx = Ctx {
        view,
        home: &home,
        principals,
    };

    // The protected-config surface for uid-1000 git: system file (unless
    // GIT_CONFIG_NOSYSTEM), the global file(s), and env-carried pairs.
    //
    // GIT_CONFIG_NOSYSTEM is a *boolean* env, read the way git reads
    // it (`git_env_bool`/`git_parse_maybe_bool`): "0", "false", "no"
    // and "off" mean the system file IS read, so a presence-only
    // check would skip `/etc/gitconfig` exactly when git reads it.
    // Only a definite true skips — an empty or unparsable value is
    // audited anyway, so a file git might read can never hide a
    // finding (a value git skips but we audit only costs a spurious
    // finding on an env that is already wrong).
    let mut files: Vec<PathBuf> = Vec::new();
    let nosystem = view
        .env("GIT_CONFIG_NOSYSTEM")
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"));
    if !nosystem {
        let path = view
            .env("GIT_CONFIG_SYSTEM")
            .unwrap_or_else(|| "/etc/gitconfig".to_string());
        if let Resolve::Done(r) = resolve_cfg_path(view, &path, &home, Path::new("/"), principals) {
            if r.in_domain {
                rep.findings.push(format!(
                    "GIT_CONFIG_SYSTEM={path} reads config from the agent domain"
                ));
            }
        }
        files.push(PathBuf::from(path));
    }
    match view.env("GIT_CONFIG_GLOBAL") {
        Some(path) => {
            if let Resolve::Done(r) =
                resolve_cfg_path(view, &path, &home, Path::new("/"), principals)
            {
                if r.in_domain {
                    rep.findings.push(format!(
                        "GIT_CONFIG_GLOBAL={path} reads config from the agent domain"
                    ));
                }
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
        scan_git_file(&ctx, &file, &mut visited, &mut rep);
    }

    // Environment-carried config: GIT_CONFIG_COUNT pairs and the
    // serialized GIT_CONFIG_PARAMETERS are protected config too.
    let mut env_pairs: Vec<(String, String, String)> = Vec::new();
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
            env_pairs.push((format!("GIT_CONFIG_KEY_{i}"), key, val));
        }
    }
    if let Some(params) = view.env("GIT_CONFIG_PARAMETERS") {
        for (key, val) in parse_config_parameters(&params) {
            env_pairs.push(("GIT_CONFIG_PARAMETERS".to_string(), key, val));
        }
    }
    for (src, key, val) in env_pairs {
        eval_cfg_entry(
            &ctx,
            Entry {
                origin: &src,
                dir: Path::new("/"),
                in_domain: false,
                key: &key,
                value: &val,
            },
            &mut visited,
            &mut rep,
        );
    }
    rep
}

/// Squote-tokenize `'key'='value'` pairs out of GIT_CONFIG_PARAMETERS.
/// Git's `sq_dequote` unescapes only `\'` and `\\` inside the quotes —
/// every other backslash stays literal.
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
                match chars.peek() {
                    Some('\'') | Some('\\') => {
                        if let Some(n) = chars.next() {
                            val.push(n);
                        }
                        continue;
                    }
                    _ => {
                        val.push('\\');
                        continue;
                    }
                }
            }
            val.push(c);
        }
        out.push((key.to_lowercase(), val));
    }
    out
}

/// Audit one config file: resolve every link hop to its landing, gate
/// the hand-off to git's parser on the bounded no-follow read, then
/// evaluate each parsed entry. `path` is a logical path — includes
/// arrive already resolved; cycle/budget bookkeeping rides `visited`
/// so an include web can neither loop nor fan out without bound.
fn scan_git_file(ctx: &Ctx, path: &Path, visited: &mut BTreeSet<PathBuf>, rep: &mut CfgAudit) {
    let view = ctx.view;
    let home = ctx.home;
    let principals = ctx.principals;
    if !visited.insert(path.to_path_buf()) {
        return;
    }
    if visited.len() > 64 {
        if visited.len() == 65 {
            rep.unverified
                .push("include chain exceeds 64 files — the tail is unverified".to_string());
        }
        return;
    }
    let r = match resolve_cfg_path(
        view,
        &path.display().to_string(),
        home,
        Path::new("/"),
        principals,
    ) {
        Resolve::Done(r) => r,
        Resolve::Unverifiable(why) => {
            rep.unverified.push(format!("{}: {why}", path.display()));
            return;
        }
    };
    if r.in_domain {
        // Config bytes the agent can write — every line is hostile,
        // whether or not we can enumerate how.
        rep.findings.push(format!(
            "{} resolves into the agent domain — agent-written config in the operator's git",
            r.path.display()
        ));
    }
    let logical = r.path.display().to_string();
    match r.terminal {
        Term::Missing => return, // absent config is inert
        Term::NotFile => {
            // A fifo/dir include would hang or refuse git at use
            // time — and we cannot audit it either.
            rep.unverified
                .push(format!("{logical}: not a regular file — unverifiable"));
            return;
        }
        Term::File => {}
    }
    // `git_config` is itself the bounded no-follow gate — a symlink,
    // a fifo, an over-cap file or an unparseable blob all come back
    // as an error, and an unaudited file fails closed.
    let entries = match view.git_config(&logical) {
        Ok(entries) => entries,
        Err(e) => {
            rep.unverified
                .push(format!("{logical}: git cannot parse it — {e}"));
            return;
        }
    };
    let file_dir = r.path.parent().unwrap_or(Path::new("/")).to_path_buf();
    for (full_key, value) in entries {
        eval_cfg_entry(
            ctx,
            Entry {
                origin: &logical,
                dir: &file_dir,
                in_domain: r.in_domain,
                key: &full_key,
                value: &value,
            },
            visited,
            rep,
        );
    }
}

fn git_config_row(view: &dyn View, operator: &str, principals: &AgentPrincipals) -> Row {
    let rep = git_config_audit(view, operator, principals);
    let level = if rep.findings.is_empty() && rep.unverified.is_empty() {
        Level::Ok
    } else {
        Level::Fail
    };
    let detail = if rep.findings.is_empty() && rep.unverified.is_empty() {
        format!("no uid-1000 git config covers {VAR_LIB}")
    } else {
        let mut d = String::new();
        if !rep.findings.is_empty() {
            let _ = write!(d, "{} violating config entrie(s)", rep.findings.len());
        }
        if !rep.unverified.is_empty() {
            if !d.is_empty() {
                d.push_str("; ");
            }
            let _ = write!(
                d,
                "{} piece(s) unverifiable — the negative cannot claim them",
                rep.unverified.len()
            );
        }
        d.push(':');
        d
    };
    let remedy = if rep.findings.is_empty() && rep.unverified.is_empty() {
        String::new()
    } else {
        let mut s = String::new();
        for f in &rep.findings {
            let _ = writeln!(s, "  {f}");
        }
        for u in &rep.unverified {
            let _ = writeln!(s, "  unverifiable: {u}");
        }
        let _ = write!(
            s,
            "remove the lines — §4 rule 2: one `safe.directory`/`include.path` covering \
             {VAR_LIB} re-opens the agent→operator code-exec channel; a piece that cannot be \
             verified fails closed, so read it or remove it"
        );
        s
    };
    row(
        "git-config",
        true,
        level,
        json!({"findings": rep.findings, "unverified": rep.unverified}),
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
